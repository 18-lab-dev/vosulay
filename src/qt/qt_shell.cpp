#include "qt_shell.h"

#include "bridge.h"
#include "vosulay/src/ffi.rs.h"

#include <LayerShellQt/Window>

#include <QAction>
#include <QApplication>
#include <QByteArray>
#include <QCommandLineOption>
#include <QCommandLineParser>
#include <QCoreApplication>
#include <QDesktopServices>
#include <QDeadlineTimer>
#include <QDir>
#include <QFile>
#include <QFileDialog>
#include <QFileInfo>
#include <QHash>
#include <QGuiApplication>
#include <QIcon>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QJsonParseError>
#include <QLocalServer>
#include <QLocalSocket>
#include <QMargins>
#include <QUuid>
#include <QMimeDatabase>
#include <QMenu>
#include <QPointer>
#include <QRegion>
#include <QScreen>
#include <QStandardPaths>
#include <QStringList>
#include <QStyle>
#include <QSystemTrayIcon>
#include <QTextStream>
#include <QThread>
#include <QTimer>
#include <QUrl>
#include <QVBoxLayout>
#include <QWebChannel>
#include <QWebEngineFrame>
#include <QWebEnginePage>
#include <QWebEngineScript>
#include <QWebEngineScriptCollection>
#include <QWebEngineSettings>
#include <QWebEngineView>
#include <QWebEngineProfile>
#include <QWebEngineUrlRequestJob>
#include <QWebEngineUrlScheme>
#include <QWebEngineUrlSchemeHandler>
#include <QWindow>

#include <algorithm>
#include <cstdint>
#include <exception>
#include <memory>
#include <optional>
#include <stdexcept>
#include <string>
#include <utility>
#include <vector>

#if defined(Q_OS_UNIX)
#include <unistd.h>
#endif

namespace vosulay {

ManagerBridge::ManagerBridge(std::function<void(const QString &)> handler,
                             QObject *parent)
    : QObject(parent), handler_(std::move(handler)) {}

void ManagerBridge::postMessage(const QString &message) { handler_(message); }

namespace {

constexpr auto kApplicationName = "vosulay";
constexpr auto kApplicationVersion = "0.1.0";
constexpr auto kChromiumUnlimitedFps = "--disable-frame-rate-limit";
constexpr auto kOverlayScheme = "vosulay-overlay";
constexpr qsizetype kMaximumIpcBytes = 4096;

QString rustStringToQString(const rust::String &value) {
  return QString::fromUtf8(value.data(), static_cast<qsizetype>(value.size()));
}

rust::Str asRustStr(const QByteArray &value) {
  return rust::Str(value.constData(), static_cast<std::size_t>(value.size()));
}

bool hasChromiumFlag(const QByteArray &flags, const QByteArray &wanted) {
  const auto tokens = flags.split(' ');
  return std::any_of(tokens.cbegin(), tokens.cend(),
                     [&wanted](const QByteArray &token) {
                       return token == wanted;
                     });
}

std::optional<int> rawFpsLimit(const QStringList &arguments) {
  for (qsizetype index = 1; index < arguments.size(); ++index) {
    const QString argument = arguments.at(index);
    if (argument == QStringLiteral("--")) {
      break;
    }
    QString value;
    if (argument == QStringLiteral("--fps-limit") ||
        argument == QStringLiteral("-f")) {
      if (++index >= arguments.size()) {
        return std::nullopt;
      }
      value = arguments.at(index);
    } else if (argument.startsWith(QStringLiteral("--fps-limit="))) {
      value = argument.sliced(QStringLiteral("--fps-limit=").size());
    } else if (argument.startsWith(QStringLiteral("-f")) &&
               argument.size() > 2) {
      value = argument.sliced(2);
      if (value.startsWith('=')) {
        value.removeFirst();
      }
    } else {
      continue;
    }

    value = value.trimmed();
    if (value.compare(QStringLiteral("auto"), Qt::CaseInsensitive) == 0) {
      return 0;
    }
    bool ok = false;
    const int fps = value.toInt(&ok);
    return ok ? std::optional<int>(fps) : std::nullopt;
  }
  return -1;
}

void configureChromium(const QStringList &arguments) {
  const auto fps = rawFpsLimit(arguments);
  if (!fps.has_value() || !(*fps == 0 || *fps > 60)) {
    return;
  }

  auto flags = qgetenv("QTWEBENGINE_CHROMIUM_FLAGS");
  if (hasChromiumFlag(flags, kChromiumUnlimitedFps)) {
    return;
  }
  if (!flags.isEmpty()) {
    flags.append(' ');
  }
  flags.append(kChromiumUnlimitedFps);
  qputenv("QTWEBENGINE_CHROMIUM_FLAGS", flags);
}


double detectedRefreshRate(QScreen *screen) {
  constexpr double fallback = 60.0;
  if (screen == nullptr || !qIsFinite(screen->refreshRate()) ||
      screen->refreshRate() < 1.0) {
    return fallback;
  }
  return qBound(30.0, screen->refreshRate(), 360.0);
}

QString limiterScript(double initialFps, double nativeRefreshRate) {
  QString source = QString::fromUtf8(R"JS(
(() => {
  const initialMaxFps = __VOSULAY_MAX_FPS__;
  const nativeFrameIntervalMs = __VOSULAY_NATIVE_FRAME_INTERVAL_MS__;
  const unboundedScheduler = __VOSULAY_UNBOUNDED_SCHEDULER__;
  if (Number.isFinite(initialMaxFps) && initialMaxFps > 0) {
    let requestedFrameRate = initialMaxFps;
    let applyFrameRate = () => {};
    let limiterInstalled = false;

    window.__vosulaySetAnimationFrameRate = (fps) => {
      requestedFrameRate = Number.isFinite(fps) && fps > 0 ? fps : 0;
      applyFrameRate(requestedFrameRate);
    };

    const installAnimationFrameLimiter = () => {
      if (limiterInstalled || typeof window.requestAnimationFrame !== "function") {
        return limiterInstalled;
      }

      let minFrameIntervalMs = requestedFrameRate > 0 ? 1000 / requestedFrameRate : 0;
      const nativeRequestAnimationFrame = window.requestAnimationFrame.bind(window);
      const nativeCancelAnimationFrame =
        typeof window.cancelAnimationFrame === "function"
          ? window.cancelAnimationFrame.bind(window)
          : null;
      const pendingAnimationFrames = new Map();
      let nextAnimationFrameId = 0;
      let scheduledAnimationFrame = null;
      let scheduledTimeout = null;
      let lastTimestamp = null;
      const frameIntervalToleranceMs = 0.5;

      const nativeFrameLeadMs = () =>
        unboundedScheduler
          ? 1
          : Math.max(1, Math.min(nativeFrameIntervalMs, minFrameIntervalMs));

      const clearScheduledTimeout = () => {
        if (scheduledTimeout === null) return;
        window.clearTimeout(scheduledTimeout);
        scheduledTimeout = null;
      };

      const clearScheduledAnimationFrame = () => {
        if (scheduledAnimationFrame === null || nativeCancelAnimationFrame === null) return;
        nativeCancelAnimationFrame(scheduledAnimationFrame);
        scheduledAnimationFrame = null;
      };

      const flushPendingAnimationFrames = (timestamp) => {
        if (pendingAnimationFrames.size === 0) return;
        if (minFrameIntervalMs > 0 && lastTimestamp !== null &&
            timestamp + frameIntervalToleranceMs - lastTimestamp < minFrameIntervalMs) {
          scheduleAnimationFramePump();
          return;
        }
        lastTimestamp = timestamp;
        const callbacks = Array.from(pendingAnimationFrames.values());
        pendingAnimationFrames.clear();
        try {
          for (const callback of callbacks) {
            try {
              callback(timestamp);
            } catch (error) {
              window.setTimeout(() => { throw error; }, 0);
            }
          }
        } finally {
          if (pendingAnimationFrames.size > 0) scheduleAnimationFramePump();
        }
      };

      const resumeAnimationFramePump = () => {
        scheduledTimeout = null;
        scheduleAnimationFramePump();
      };

      const flushNativeAnimationFrames = (timestamp) => {
        scheduledAnimationFrame = null;
        flushPendingAnimationFrames(timestamp);
      };

      const scheduleAnimationFramePump = () => {
        if (pendingAnimationFrames.size === 0 ||
            scheduledAnimationFrame !== null || scheduledTimeout !== null) return;
        if (minFrameIntervalMs > 0 && lastTimestamp !== null) {
          const remainingMs = lastTimestamp + minFrameIntervalMs - performance.now();
          const delayMs = remainingMs - nativeFrameLeadMs();
          if (delayMs > frameIntervalToleranceMs) {
            scheduledTimeout = window.setTimeout(resumeAnimationFramePump, delayMs);
            return;
          }
        }
        scheduledAnimationFrame = nativeRequestAnimationFrame(flushNativeAnimationFrames);
      };

      applyFrameRate = (fps) => {
        clearScheduledTimeout();
        clearScheduledAnimationFrame();
        minFrameIntervalMs = Number.isFinite(fps) && fps > 0 ? 1000 / fps : 0;
        lastTimestamp = null;
        if (pendingAnimationFrames.size > 0) scheduleAnimationFramePump();
      };

      window.requestAnimationFrame = (callback) => {
        const animationFrameId = ++nextAnimationFrameId;
        pendingAnimationFrames.set(animationFrameId, callback);
        scheduleAnimationFramePump();
        return animationFrameId;
      };

      window.cancelAnimationFrame = (animationFrameId) => {
        pendingAnimationFrames.delete(animationFrameId);
        if (pendingAnimationFrames.size === 0) {
          clearScheduledTimeout();
          clearScheduledAnimationFrame();
        }
      };

      limiterInstalled = true;
      applyFrameRate(requestedFrameRate);
      return true;
    };

    if (!installAnimationFrameLimiter()) {
      const deferredInstall = () => installAnimationFrameLimiter();
      document.addEventListener("DOMContentLoaded", deferredInstall, { once: true });
      window.addEventListener("load", deferredInstall, { once: true });
      window.setTimeout(deferredInstall, 0);
    }
  }
})();
)JS");
  source.replace(QStringLiteral("__VOSULAY_MAX_FPS__"),
                 QString::number(initialFps, 'f', 3));
  source.replace(QStringLiteral("__VOSULAY_NATIVE_FRAME_INTERVAL_MS__"),
                 QString::number(1000.0 / nativeRefreshRate, 'f', 3));
  source.replace(
      QStringLiteral("__VOSULAY_UNBOUNDED_SCHEDULER__"),
      hasChromiumFlag(qgetenv("QTWEBENGINE_CHROMIUM_FLAGS"),
                      kChromiumUnlimitedFps)
          ? QStringLiteral("true")
          : QStringLiteral("false"));
  return source;
}

QString managerBridgeScript() {
  return QString::fromUtf8(R"JS(
(() => {
  const queuedMessages = [];
  const documentId =
    globalThis.crypto?.randomUUID?.() ??
    `${performance.timeOrigin}:${Math.random()}`;
  const serializePayload = (payload) => {
    if (typeof payload !== "string") {
      payload = JSON.stringify(payload);
    }
    try {
      const message = JSON.parse(payload);
      if (message && message.type === "ready") {
        message.__vosulay_document_id = documentId;
        return JSON.stringify(message);
      }
    } catch (_) {
    }
    return payload;
  };
  let sender = (payload) => queuedMessages.push(payload);
  window.__VOSULAY_POST_MESSAGE__ = (payload) => sender(payload);
  window.ipc = { postMessage: (payload) => window.__VOSULAY_POST_MESSAGE__(payload) };

  const initializeChannel = () => {
    const script = document.createElement("script");
    script.onload = () => {
      new QWebChannel(qt.webChannelTransport, (channel) => {
        const bridge = channel.objects.vosulayBridge;
        sender = (payload) => bridge.postMessage(serializePayload(payload));
        for (const payload of queuedMessages.splice(0)) sender(payload);
        window.dispatchEvent(new Event("vosulay-bridge-ready"));
      });
    };
    script.src = "qrc:///qtwebchannel/qwebchannel.js";
    (document.head || document.documentElement).appendChild(script);
  };

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", initializeChannel, { once: true });
  } else {
    initializeChannel();
  }
})();
)JS");
}

QString counterMetadataScript(const QUrl &baseUrl) {
  const QByteArray encodedBaseUrl =
      QJsonDocument(QJsonArray{baseUrl.toString(QUrl::FullyEncoded)})
          .toJson(QJsonDocument::Compact);
  QString source = QString::fromUtf8(R"JS(
(() => {
  try {
    const vosuOrigin = new URL(__VOSULAY_BASE_URL__[0]).origin;
    if (window.location.origin !== vosuOrigin ||
        typeof window.COUNTER_PATH === "string") {
      return;
    }

    const pathname = window.location.pathname.endsWith("/")
      ? window.location.pathname
      : window.location.pathname.slice(0, window.location.pathname.lastIndexOf("/") + 1);
    const counterPath = pathname
      .split("/")
      .filter(Boolean)
      .map((segment) => decodeURIComponent(segment))
      .join("/");
    if (counterPath) window.COUNTER_PATH = counterPath;
  } catch (_) {
  }
})();
)JS");
  return source.replace(QStringLiteral("__VOSULAY_BASE_URL__"),
                        QString::fromUtf8(encodedBaseUrl));
}

void installScript(QWebEnginePage *page, const QString &name,
                   const QString &source, bool subframes) {
  QWebEngineScript script;
  script.setName(name);
  script.setInjectionPoint(QWebEngineScript::DocumentCreation);
  script.setRunsOnSubFrames(subframes);
  script.setWorldId(QWebEngineScript::MainWorld);
  script.setSourceCode(source);
  page->scripts().insert(script);
}

void updateFrameRate(QWebEngineFrame frame, double fps) {
  frame.runJavaScript(
      QStringLiteral("if (typeof window.__vosulaySetAnimationFrameRate === "
                     "'function') window.__vosulaySetAnimationFrameRate(%1);")
          .arg(QString::number(fps, 'f', 3)));
  for (const auto &child : frame.children()) {
    updateFrameRate(child, fps);
  }
}

QString socketName() {
  QString directory = QStandardPaths::writableLocation(QStandardPaths::RuntimeLocation);
  if (directory.isEmpty()) {
    directory = QDir::tempPath();
  }
#if defined(Q_OS_UNIX)
  const auto user = static_cast<qulonglong>(::getuid());
#else
  const auto user = static_cast<qulonglong>(0);
#endif
  return QDir(directory).filePath(
      QStringLiteral("vosulay-%1.sock").arg(user));
}

QJsonObject errorEnvelope(const QString &message) {
  return QJsonObject{{QStringLiteral("type"), QStringLiteral("error")},
                     {QStringLiteral("message"), message}};
}

bool isOverlayToken(const QString &token) {
  return token.size() == 32 &&
         std::all_of(token.cbegin(), token.cend(), [](QChar character) {
           return character.isDigit() ||
                  (character >= QLatin1Char('a') &&
                   character <= QLatin1Char('f'));
         });
}

bool isPathInsideRoot(const QString &root, const QString &target) {
  const QString relative = QDir(root).relativeFilePath(target);
  return !QDir::isAbsolutePath(relative) && relative != QStringLiteral("..") &&
         !relative.startsWith(QStringLiteral("../"));
}

std::optional<QString> overlayRootFromUrl(const QUrl &url) {
  if (url.scheme() != QString::fromLatin1(kOverlayScheme) ||
      !isOverlayToken(url.host()) || url.port() != -1) {
    return std::nullopt;
  }

  const QString path = url.path(QUrl::FullyDecoded);
  const qsizetype rootEnd = path.indexOf('/', 1);
  if (!path.startsWith('/') || rootEnd <= 1 || rootEnd == path.size() - 1) {
    return std::nullopt;
  }

  const QByteArray encodedRoot = path.sliced(1, rootEnd - 1).toLatin1();
  const auto decodedRoot = QByteArray::fromBase64Encoding(
      encodedRoot, QByteArray::Base64UrlEncoding |
                       QByteArray::AbortOnBase64DecodingErrors);
  if (!decodedRoot ||
      decodedRoot.decoded.toBase64(QByteArray::Base64UrlEncoding |
                                   QByteArray::OmitTrailingEquals) !=
          encodedRoot) {
    return std::nullopt;
  }

  const QByteArray rootBytes = decodedRoot.decoded;
  const QString decodedRootPath = QString::fromUtf8(rootBytes);
  if (decodedRootPath.toUtf8() != rootBytes) {
    return std::nullopt;
  }

  const QFileInfo rootInfo(decodedRootPath);
  const QString root = rootInfo.canonicalFilePath();
  if (root.isEmpty() || !rootInfo.isDir() || root != decodedRootPath) {
    return std::nullopt;
  }
  return root;
}

QUrl overlayUrl(const QFileInfo &entry) {
  const QString root = entry.dir().canonicalPath();
  const QString target = entry.canonicalFilePath();
  if (root.isEmpty() || target.isEmpty() || !isPathInsideRoot(root, target)) {
    return {};
  }

  const QByteArray rootBytes = root.toUtf8();
  const QByteArray encodedRoot =
      rootBytes.toBase64(QByteArray::Base64UrlEncoding |
                         QByteArray::OmitTrailingEquals);
  QUrl url;
  url.setScheme(QString::fromLatin1(kOverlayScheme));
  url.setHost(QUuid::createUuid()
                  .toString(QUuid::WithoutBraces)
                  .remove(QLatin1Char('-')));
  url.setPath(QStringLiteral("/%1/%2")
                  .arg(QString::fromLatin1(encodedRoot), entry.fileName()));
  return url;
}

class OverlaySchemeHandler final : public QWebEngineUrlSchemeHandler {
public:
  explicit OverlaySchemeHandler(QObject *parent)
      : QWebEngineUrlSchemeHandler(parent) {}

  bool authorizeUrl(const QUrl &url) {
    const auto root = overlayRootFromUrl(url);
    if (!root) {
      return false;
    }
    const auto existing = authorizedRoots_.constFind(url.host());
    if (existing != authorizedRoots_.cend() && *existing != *root) {
      return false;
    }
    authorizedRoots_.insert(url.host(), *root);
    return true;
  }

  void requestStarted(QWebEngineUrlRequestJob *job) override {
    const QUrl url = job->requestUrl();
    const auto root = overlayRootFromUrl(url);
    if (!root) {
      job->fail(QWebEngineUrlRequestJob::UrlInvalid);
      return;
    }

    const auto authorizedRoot = authorizedRoots_.constFind(url.host());
    if (authorizedRoot == authorizedRoots_.cend() || *authorizedRoot != *root) {
      job->fail(QWebEngineUrlRequestJob::RequestDenied);
      return;
    }

    const QString path = url.path(QUrl::FullyDecoded);
    const qsizetype rootEnd = path.indexOf('/', 1);

    const QString relativePath = path.sliced(rootEnd + 1);
    const QFileInfo requestedInfo(QDir(*root).filePath(relativePath));
    const QString target = requestedInfo.canonicalFilePath();
    if (target.isEmpty()) {
      job->fail(QWebEngineUrlRequestJob::UrlNotFound);
      return;
    }
    if (!isPathInsideRoot(*root, target)) {
      job->fail(QWebEngineUrlRequestJob::RequestDenied);
      return;
    }

    const QFileInfo targetInfo(target);
    if (!targetInfo.isFile() || !targetInfo.isReadable()) {
      job->fail(QWebEngineUrlRequestJob::RequestDenied);
      return;
    }

    auto *file = new QFile(target, job);
    if (!file->open(QIODevice::ReadOnly)) {
      job->fail(QWebEngineUrlRequestJob::RequestFailed);
      return;
    }
    const QByteArray mimeType =
        QMimeDatabase().mimeTypeForFile(targetInfo).name().toUtf8();
    job->reply(mimeType, file);
  }

private:
  QHash<QString, QString> authorizedRoots_;
};

void registerOverlayScheme() {
  QWebEngineUrlScheme scheme{QByteArray(kOverlayScheme)};
  scheme.setSyntax(QWebEngineUrlScheme::Syntax::HostAndPort);
  scheme.setDefaultPort(443);
  scheme.setFlags(QWebEngineUrlScheme::SecureScheme |
                  QWebEngineUrlScheme::CorsEnabled |
                  QWebEngineUrlScheme::FetchApiAllowed);
  QWebEngineUrlScheme::registerScheme(scheme);
}

struct ParsedOptions {
  QString vosuUrl;
  QScreen *screen = nullptr;
  int fpsLimit = -1;
  bool attach = false;
};

class ShellWindow final : public QWidget {
public:
  explicit ShellWindow(ParsedOptions options)
      : options_(std::move(options)), server_(this), tray_(this) {
    setAttribute(Qt::WA_TranslucentBackground);
    setWindowFlags(Qt::FramelessWindowHint | Qt::NoDropShadowWindowHint |
                   Qt::WindowStaysOnTopHint | Qt::X11BypassWindowManagerHint);

    auto *layout = new QVBoxLayout(this);
    layout->setContentsMargins(0, 0, 0, 0);
    view_ = new QWebEngineView(this);
    view_->setAttribute(Qt::WA_TranslucentBackground);
    view_->setContextMenuPolicy(Qt::NoContextMenu);
    layout->addWidget(view_);

    page_ = new QWebEnginePage(view_);
    page_->setBackgroundColor(Qt::transparent);
    view_->setPage(page_);
    overlaySchemeHandler_ = new OverlaySchemeHandler(page_->profile());
    page_->profile()->installUrlSchemeHandler(QByteArray(kOverlayScheme),
                                              overlaySchemeHandler_);
    page_->settings()->setAttribute(
        QWebEngineSettings::LocalContentCanAccessFileUrls, false);
    page_->settings()->setAttribute(
        QWebEngineSettings::LocalContentCanAccessRemoteUrls, true);

    installScript(page_, QStringLiteral("vosulay-counter-metadata"),
                  counterMetadataScript(QUrl(options_.vosuUrl)), true);

    if (options_.fpsLimit >= 0) {
      installLimiter();
    }
    installScript(page_, QStringLiteral("vosulay-manager-bridge"),
                  managerBridgeScript(), false);

    channel_ = new QWebChannel(page_);
    bridge_ = new ManagerBridge(
        [this](const QString &message) { handleManagerMessage(message); },
        channel_);
    channel_->registerObject(QStringLiteral("vosulayBridge"), bridge_);
    page_->setWebChannel(channel_);

    connect(windowHandleOrCreate(), &QWindow::screenChanged, this,
            [this](QScreen *screen) {
              options_.screen = screen;
              connectRefreshRateSignal(screen);
              syncAutoFrameRate();
            });
    connectRefreshRateSignal(options_.screen);

    setupTray();
    setupIpcServer();

    const auto html = rustStringToQString(
        manager_document_for_qt(static_cast<std::int32_t>(options_.fpsLimit)));
    view_->setHtml(html, QUrl(QStringLiteral("qrc:///vosulay/manager.html")));
  }

  ~ShellWindow() override {
    shuttingDown_ = true;
    ++fetchGeneration_;
    if (fetchWorker_ != nullptr) {
      fetchWorker_->quit();
      fetchWorker_->wait();
      delete fetchWorker_;
      fetchWorker_ = nullptr;
    }
    if (saveWorker_ != nullptr) {
      saveWorker_->quit();
      saveWorker_->wait();
      delete saveWorker_;
      saveWorker_ = nullptr;
    }
    if (pendingSave_.has_value()) {
      const QByteArray json = std::move(pendingSave_->json);
      pendingSave_.reset();
      QThread *worker = QThread::create([json] {
        try {
          save_config_for_qt(asRustStr(json));
        } catch (const std::exception &) {
        }
      });
      worker->start();
      worker->wait();
      delete worker;
    }
  }

  bool ipcReady() const { return ipcReady_; }

  void initializeWindow() {
    QScreen *screen = options_.screen != nullptr ? options_.screen
                                                 : QApplication::primaryScreen();
    options_.screen = screen;
    if (screen != nullptr) {
      windowHandleOrCreate()->setScreen(screen);
      setGeometry(screen->geometry());
    }

    if (QApplication::platformName() == QStringLiteral("wayland")) {
      layerWindow_ = LayerShellQt::Window::get(windowHandleOrCreate());
      if (layerWindow_ != nullptr) {
        layerWindow_->setExclusiveZone(-1);
        layerWindow_->setScope(QStringLiteral("vosulay"));
        layerWindow_->setLayer(LayerShellQt::Window::LayerOverlay);
        layerWindow_->setScreen(screen);
        layerWindow_->setKeyboardInteractivity(
            LayerShellQt::Window::KeyboardInteractivityNone);
      }
    }

    setEditing(false);
    showMaximized();
    tray_.show();
  }

private:
  QWindow *windowHandleOrCreate() {
    createWinId();
    return windowHandle();
  }

  void setupTray() {
    QIcon icon = QIcon::fromTheme(QStringLiteral("applications-games"));
    if (icon.isNull()) {
      icon = style()->standardIcon(QStyle::SP_ComputerIcon);
    }
    tray_.setIcon(icon);
    tray_.setToolTip(QStringLiteral("vosulay"));

    auto *menu = new QMenu(this);
    editAction_ = menu->addAction(QStringLiteral("Edit vosulay"));
    visibilityAction_ = menu->addAction(QStringLiteral("Hide vosulay"));
    auto *reloadAction = menu->addAction(QStringLiteral("Reload vosulay"));
    menu->addSeparator();
    auto *quitAction = menu->addAction(QStringLiteral("Quit vosulay"));
    tray_.setContextMenu(menu);

    connect(editAction_, &QAction::triggered, this,
            [this] { setEditing(!editing_); });
    connect(visibilityAction_, &QAction::triggered, this,
            [this] { setOverlayVisibility(!isVisible()); });
    connect(reloadAction, &QAction::triggered, this,
            [this] { reloadManager(); });
    connect(quitAction, &QAction::triggered, qApp, &QApplication::quit);
    connect(&tray_, &QSystemTrayIcon::activated, this,
            [this](QSystemTrayIcon::ActivationReason reason) {
              if (reason == QSystemTrayIcon::Trigger) {
                setEditing(!editing_);
              }
            });
  }

  void setOverlayVisibility(bool visible) {
    setVisible(visible);
    visibilityAction_->setText(visible ? QStringLiteral("Hide vosulay")
                                       : QStringLiteral("Show vosulay"));
    updatePageLifecycle();
  }

  void reloadManager() {
    if (page_->lifecycleState() ==
        QWebEnginePage::LifecycleState::Discarded) {
      page_->setLifecycleState(QWebEnginePage::LifecycleState::Active);
      view_->show();
    }
    view_->reload();
  }

  void updatePageLifecycle() {
    using LifecycleState = QWebEnginePage::LifecycleState;
    LifecycleState desired = LifecycleState::Active;
    const bool canDiscard =
        editingOffApplied_ && saveWorker_ == nullptr &&
        !pendingSave_.has_value() && !persistenceError_;
    if (!editing_ && !activeProfileHasOverlays_ && canDiscard) {
      desired = LifecycleState::Discarded;
    } else if (!editing_ && !persistenceError_ && editingOffApplied_ &&
               !isVisible()) {
      desired = LifecycleState::Frozen;
    }

    if (page_->lifecycleState() == desired) {
      return;
    }
    if (desired == LifecycleState::Discarded) {
      ++fetchGeneration_;
      fetchPending_ = false;
      view_->hide();
      page_->setLifecycleState(desired);
      return;
    }
    page_->setLifecycleState(desired);
    if (desired == LifecycleState::Active) {
      view_->show();
    }
  }

  void setEditing(bool editing) {
    editing_ = editing;
    if (editing) {
      updatePageLifecycle();
    } else {
      editingOffApplied_ = false;
    }
    QWindow *window = windowHandleOrCreate();
    if (QApplication::platformName() == QStringLiteral("xcb")) {
      window->setFlag(Qt::WindowTransparentForInput, !editing);
    } else if (editing) {
      window->setMask(QRegion());
    } else {
      window->setMask(QRegion(-1, -1, 1, 1));
    }

    if (layerWindow_ != nullptr) {
      layerWindow_->setKeyboardInteractivity(
          editing ? LayerShellQt::Window::KeyboardInteractivityExclusive
                  : LayerShellQt::Window::KeyboardInteractivityNone);
    }
    if (editAction_ != nullptr) {
      editAction_->setText(editing ? QStringLiteral("Finish editing vosulay")
                                   : QStringLiteral("Edit vosulay"));
    }
    sendEnvelope(QJsonObject{{QStringLiteral("type"),
                              QStringLiteral("editing")},
                             {QStringLiteral("editing"), editing}});
    if (editing &&
        initializationAwaitingEditingAppliedDocumentId_.has_value()) {
      initializationAwaitingEditingAppliedDocumentId_.reset();
      fetchCounters();
    }
  }

  void setOverlayGeometry(const QRect &geometry) {
    if (!geometry.isValid()) {
      return;
    }
    QScreen *screen = QGuiApplication::screenAt(geometry.center());
    if (screen == nullptr) {
      screen = options_.screen;
    }
    if (screen != nullptr && screen != options_.screen) {
      options_.screen = screen;
      windowHandleOrCreate()->setScreen(screen);
      if (layerWindow_ != nullptr) {
        layerWindow_->setScreen(screen);
      }
      connectRefreshRateSignal(screen);
      syncAutoFrameRate();
    }

    if (layerWindow_ != nullptr && screen != nullptr) {
      const QRect screenGeometry = screen->geometry();
      const int left = geometry.left() - screenGeometry.left();
      const int top = geometry.top() - screenGeometry.top();
      const int right = screenGeometry.right() - geometry.right();
      const int bottom = screenGeometry.bottom() - geometry.bottom();
      layerWindow_->setMargins(QMargins(left, top, right, bottom));
      layerWindow_->setDesiredSize(geometry.size());
    }
    setGeometry(geometry);
    update();
  }

  void queueOverlayGeometry(const QRect &geometry) {
    if ((pendingGeometry_.has_value() && *pendingGeometry_ == geometry) ||
        (!pendingGeometry_.has_value() &&
         ((appliedGeometry_.has_value() && *appliedGeometry_ == geometry) ||
          QWidget::geometry() == geometry))) {
      return;
    }
    pendingGeometry_ = geometry;
    if (geometryApplyScheduled_) {
      return;
    }
    geometryApplyScheduled_ = true;
    QTimer::singleShot(0, this, [this] {
      geometryApplyScheduled_ = false;
      if (!pendingGeometry_.has_value()) {
        return;
      }
      const QRect geometry = std::move(*pendingGeometry_);
      pendingGeometry_.reset();
      if ((!appliedGeometry_.has_value() || *appliedGeometry_ != geometry) &&
          QWidget::geometry() != geometry) {
        setOverlayGeometry(geometry);
        appliedGeometry_ = geometry;
      }
    });
  }

  void installLimiter() {
    const double nativeRefreshRate = detectedRefreshRate(options_.screen);
    const double fps = options_.fpsLimit == 0
                           ? nativeRefreshRate
                           : static_cast<double>(options_.fpsLimit);
    installScript(page_, QStringLiteral("vosulay-animation-frame-limiter"),
                  limiterScript(fps, nativeRefreshRate), true);
  }

  void connectRefreshRateSignal(QScreen *screen) {
    if (refreshScreen_ == screen) {
      return;
    }
    if (refreshScreen_ != nullptr) {
      disconnect(refreshScreen_, nullptr, this, nullptr);
    }
    refreshScreen_ = screen;
    if (refreshScreen_ != nullptr) {
      connect(refreshScreen_, &QScreen::refreshRateChanged, this,
              [this] { syncAutoFrameRate(); });
    }
  }

  void syncAutoFrameRate() {
    if (options_.fpsLimit != 0 || page_ == nullptr) {
      return;
    }
    const double fps = detectedRefreshRate(options_.screen);
    const auto oldScripts =
        page_->scripts().find(QStringLiteral("vosulay-animation-frame-limiter"));
    for (const auto &oldScript : oldScripts) {
      page_->scripts().remove(oldScript);
    }
    installScript(page_, QStringLiteral("vosulay-animation-frame-limiter"),
                  limiterScript(fps, fps), true);
    updateFrameRate(page_->mainFrame(), fps);
  }

  void sendEnvelope(const QJsonObject &envelope) {
    const QString payload = QString::fromUtf8(
        QJsonDocument(envelope).toJson(QJsonDocument::Compact));
    const QString javascript =
        QStringLiteral("if (typeof window.__VOSULAY_RECEIVE__ === 'function') "
                       "window.__VOSULAY_RECEIVE__(%1); else if "
                       "(window.vosulay && typeof window.vosulay.receive === "
                       "'function') window.vosulay.receive(%1);")
            .arg(payload);
    page_->runJavaScript(javascript);
  }

  void handleManagerMessage(const QString &message) {
    QJsonParseError parseError;
    const QJsonDocument document =
        QJsonDocument::fromJson(message.toUtf8(), &parseError);
    if (parseError.error != QJsonParseError::NoError || !document.isObject()) {
      sendEnvelope(errorEnvelope(QStringLiteral("Invalid message from vosulay manager")));
      return;
    }

    const QJsonObject request = document.object();
    const QString type = request.value(QStringLiteral("type")).toString();
    if (type == QStringLiteral("ready")) {
      const QString documentId =
          request.value(QStringLiteral("__vosulay_document_id")).toString();
      if (managerDocumentId_.has_value() &&
          *managerDocumentId_ == documentId) {
        return;
      }
      managerDocumentId_ = documentId;
      if (saveWorker_ != nullptr || pendingSave_.has_value()) {
        ++fetchGeneration_;
        fetchPending_ = false;
        pendingInitializationDocumentId_ = documentId;
      } else {
        initializeManagerDocument(documentId);
      }
    } else if (type == QStringLiteral("save-config")) {
      const QJsonValue saveIdValue =
          request.value(QStringLiteral("save_id"));
      if (!saveIdValue.isString() || saveIdValue.toString().isEmpty()) {
        sendEnvelope(errorEnvelope(QStringLiteral("Invalid vosulay save ID")));
        return;
      }
      saveConfiguration(request.value(QStringLiteral("config")),
                        saveIdValue.toString());
    } else if (type == QStringLiteral("refresh-counters")) {
      fetchCounters();
    } else if (type == QStringLiteral("editing-applied")) {
      if (!request.value(QStringLiteral("editing")).toBool(true) &&
          !editing_) {
        editingOffApplied_ = true;
        updatePageLifecycle();
        if (initializationAwaitingEditingAppliedDocumentId_.has_value() &&
            managerDocumentId_.has_value() &&
            *initializationAwaitingEditingAppliedDocumentId_ ==
                *managerDocumentId_) {
          initializationAwaitingEditingAppliedDocumentId_.reset();
          if (page_->lifecycleState() !=
              QWebEnginePage::LifecycleState::Discarded) {
            fetchCounters();
          }
        }
      }
    } else if (type == QStringLiteral("set-editing")) {
      setEditing(request.value(QStringLiteral("editing")).toBool());
    } else if (type == QStringLiteral("choose-overlay-file")) {
      chooseOverlayFile();
    } else if (type == QStringLiteral("open-settings")) {
      openSettings(request.value(QStringLiteral("counter_id")).toString());
    } else if (type == QStringLiteral("open-available")) {
      openAvailable();
    } else if (type == QStringLiteral("quit")) {
      QMetaObject::invokeMethod(qApp, &QApplication::quit,
                                Qt::QueuedConnection);
    } else {
      sendEnvelope(errorEnvelope(QStringLiteral("Unknown vosulay manager message")));
    }
  }

  void chooseOverlayFile() {
    const QString path = QFileDialog::getOpenFileName(
        this, QStringLiteral("Choose HTML overlay"), QString(),
        QStringLiteral("HTML files (*.html *.htm)"));
    if (path.isEmpty()) {
      return;
    }

    const QFileInfo fileInfo(path);
    const QString suffix = fileInfo.suffix();
    QFile file(fileInfo.absoluteFilePath());
    if (!fileInfo.isFile() || !fileInfo.isReadable() ||
        (suffix.compare(QStringLiteral("html"), Qt::CaseInsensitive) != 0 &&
         suffix.compare(QStringLiteral("htm"), Qt::CaseInsensitive) != 0) ||
        !file.open(QIODevice::ReadOnly)) {
      sendEnvelope(
          errorEnvelope(QStringLiteral("Selected overlay is not a readable HTML file")));
      return;
    }

    const QUrl selectedUrl = overlayUrl(fileInfo);
    if (!selectedUrl.isValid() || selectedUrl.isEmpty()) {
      sendEnvelope(errorEnvelope(
          QStringLiteral("Selected overlay could not be resolved safely")));
      return;
    }

    if (!overlaySchemeHandler_->authorizeUrl(selectedUrl)) {
      sendEnvelope(errorEnvelope(
          QStringLiteral("Selected overlay could not be authorized safely")));
      return;
    }

    sendEnvelope(
        QJsonObject{{QStringLiteral("type"), QStringLiteral("overlay-file")},
                    {QStringLiteral("url"), selectedUrl.toString()},
                    {QStringLiteral("name"), fileInfo.completeBaseName()}});
  }

  static bool configHasActiveOverlays(const QJsonValue &config) {
    if (!config.isObject()) {
      return true;
    }
    const QJsonObject object = config.toObject();
    const QJsonValue activeProfileValue =
        object.value(QStringLiteral("active_profile"));
    const QJsonValue profilesValue = object.value(QStringLiteral("profiles"));
    if (!activeProfileValue.isString() ||
        activeProfileValue.toString().isEmpty() || !profilesValue.isArray()) {
      return true;
    }

    const QString activeProfile = activeProfileValue.toString();
    std::optional<bool> activeProfileHasOverlays;
    for (const QJsonValue &profile : profilesValue.toArray()) {
      if (!profile.isObject()) {
        return true;
      }
      const QJsonObject profileObject = profile.toObject();
      const QJsonValue id = profileObject.value(QStringLiteral("id"));
      const QJsonValue overlays =
          profileObject.value(QStringLiteral("overlays"));
      if (!id.isString() || id.toString().isEmpty() || !overlays.isArray()) {
        return true;
      }
      if (id.toString() == activeProfile) {
        if (activeProfileHasOverlays.has_value()) {
          return true;
        }
        activeProfileHasOverlays = !overlays.toArray().isEmpty();
      }
    }
    return activeProfileHasOverlays.value_or(true);
  }

  void loadConfiguration() {
    try {
      const QString response = rustStringToQString(load_config_for_qt());
      QJsonParseError error;
      const auto document = QJsonDocument::fromJson(response.toUtf8(), &error);
      if (error.error != QJsonParseError::NoError || !document.isObject()) {
        throw std::runtime_error("invalid configuration response from vosulay");
      }
      const auto object = document.object();
      const QJsonValue config = object.value(QStringLiteral("config"));
      activeProfileHasOverlays_ = configHasActiveOverlays(config);
      if (config.isObject()) {
        const QJsonArray profiles =
            config.toObject().value(QStringLiteral("profiles")).toArray();
        for (const QJsonValue &profile : profiles) {
          const QJsonArray overlays =
              profile.toObject().value(QStringLiteral("overlays")).toArray();
          for (const QJsonValue &overlay : overlays) {
            const QUrl url(
                overlay.toObject().value(QStringLiteral("url")).toString());
            if (url.scheme() == QString::fromLatin1(kOverlayScheme)) {
              overlaySchemeHandler_->authorizeUrl(url);
            }
          }
        }
      }
      sendEnvelope(QJsonObject{{QStringLiteral("type"),
                                QStringLiteral("config")},
                               {QStringLiteral("config"), config}});
      const auto loadError = object.value(QStringLiteral("error"));
      const bool hasLoadError =
          loadError.isString() && !loadError.toString().isEmpty();
      persistenceError_ = hasLoadError;
      if (hasLoadError) {
        sendEnvelope(errorEnvelope(loadError.toString()));
      }
    } catch (const std::exception &error) {
      persistenceError_ = true;
      sendEnvelope(errorEnvelope(QString::fromUtf8(error.what())));
    }
  }

  struct WorkerResult {
    QString payload;
    QString error;
  };

  struct SaveRequest {
    QByteArray json;
    QString saveId;
    bool activeProfileHasOverlays = true;
  };

  void initializeManagerDocument(const QString &documentId) {
    if (!managerDocumentId_.has_value() ||
        *managerDocumentId_ != documentId) {
      return;
    }
    pendingInitializationDocumentId_.reset();
    initializationAwaitingEditingAppliedDocumentId_.reset();
    loadConfiguration();
    if (editing_) {
      updatePageLifecycle();
    } else {
      editingOffApplied_ = false;
      initializationAwaitingEditingAppliedDocumentId_ = documentId;
    }
    sendEnvelope(QJsonObject{{QStringLiteral("type"),
                              QStringLiteral("editing")},
                             {QStringLiteral("editing"), editing_}});
    if (editing_) {
      fetchCounters();
    }
  }

  void initializePendingManagerDocument() {
    if (!pendingInitializationDocumentId_.has_value()) {
      return;
    }
    const QString documentId =
        std::move(*pendingInitializationDocumentId_);
    pendingInitializationDocumentId_.reset();
    initializeManagerDocument(documentId);
  }

  void saveConfiguration(const QJsonValue &config, QString saveId) {
    if (!config.isObject()) {
      sendEnvelope(errorEnvelope(QStringLiteral("Invalid vosulay configuration")));
      return;
    }
    const QJsonObject configObject = config.toObject();
    SaveRequest save{
        QJsonDocument(configObject).toJson(QJsonDocument::Compact),
        std::move(saveId), configHasActiveOverlays(configObject)};
    if (saveWorker_ != nullptr) {
      pendingSave_ = std::move(save);
      return;
    }
    startSave(std::move(save));
  }

  void startSave(SaveRequest save) {
    auto result = std::make_shared<WorkerResult>();
    const QString saveId = save.saveId;
    const bool savedActiveProfileHasOverlays =
        save.activeProfileHasOverlays;
    QThread *worker =
        QThread::create([json = std::move(save.json), result] {
          try {
            save_config_for_qt(asRustStr(json));
          } catch (const std::exception &error) {
            result->error = QString::fromUtf8(error.what());
          }
        });
    saveWorker_ = worker;
    worker->setParent(this);
    connect(worker, &QThread::finished, this,
            [this, worker, result, saveId, savedActiveProfileHasOverlays] {
      if (saveWorker_ == worker) {
        saveWorker_ = nullptr;
      }
      worker->deleteLater();
      if (shuttingDown_) {
        return;
      }
      if (pendingSave_.has_value()) {
        if (!result->error.isEmpty()) {
          sendEnvelope(errorEnvelope(result->error));
        } else {
          sendEnvelope(QJsonObject{{QStringLiteral("type"),
                                    QStringLiteral("saved")},
                                   {QStringLiteral("save_id"), saveId}});
        }
        SaveRequest pending = std::move(*pendingSave_);
        pendingSave_.reset();
        startSave(std::move(pending));
        return;
      }
      if (!result->error.isEmpty()) {
        persistenceError_ = true;
        initializePendingManagerDocument();
        updatePageLifecycle();
        sendEnvelope(errorEnvelope(result->error));
      } else {
        activeProfileHasOverlays_ = savedActiveProfileHasOverlays;
        persistenceError_ = false;
        sendEnvelope(QJsonObject{{QStringLiteral("type"),
                                  QStringLiteral("saved")},
                                 {QStringLiteral("save_id"), saveId}});
        initializePendingManagerDocument();
        updatePageLifecycle();
      }
    });
    worker->start();
  }

  void fetchCounters() {
    const quint64 generation = ++fetchGeneration_;
    if (fetchWorker_ != nullptr) {
      fetchPending_ = true;
      return;
    }
    startCounterFetch(generation);
  }

  void startCounterFetch(quint64 generation) {
    const QByteArray baseUrl = options_.vosuUrl.toUtf8();
    auto result = std::make_shared<WorkerResult>();
    QThread *worker = QThread::create([baseUrl, result] {
      try {
        result->payload =
            rustStringToQString(fetch_counters_for_qt(asRustStr(baseUrl)));
      } catch (const std::exception &error) {
        result->error = QString::fromUtf8(error.what());
      }
    });
    fetchWorker_ = worker;
    worker->setParent(this);
    connect(worker, &QThread::finished, this,
            [this, worker, generation, result] {
              if (fetchWorker_ == worker) {
                fetchWorker_ = nullptr;
              }
              worker->deleteLater();
              if (shuttingDown_) {
                return;
              }
              if (generation == fetchGeneration_) {
                if (!result->error.isEmpty()) {
                  sendEnvelope(errorEnvelope(result->error));
                } else {
                  QJsonParseError parseError;
                  const auto document =
                      QJsonDocument::fromJson(result->payload.toUtf8(),
                                              &parseError);
                  if (parseError.error != QJsonParseError::NoError ||
                      !document.isArray()) {
                    sendEnvelope(errorEnvelope(
                        QStringLiteral("Invalid counter catalog from vosu")));
                  } else {
                    sendEnvelope(QJsonObject{
                        {QStringLiteral("type"), QStringLiteral("counters")},
                        {QStringLiteral("counters"), document.array()}});
                  }
                }
              }
              if (fetchPending_) {
                fetchPending_ = false;
                startCounterFetch(fetchGeneration_);
              }
            });
    worker->start();
  }

  void openSettings(const QString &counterId) {
    if (counterId.isEmpty()) {
      sendEnvelope(errorEnvelope(QStringLiteral("A vosu counter is required")));
      return;
    }
    try {
      const QByteArray baseUrl = options_.vosuUrl.toUtf8();
      const QByteArray id = counterId.toUtf8();
      const QUrl url(rustStringToQString(
          settings_url_for_qt(asRustStr(baseUrl), asRustStr(id))));
      if (!QDesktopServices::openUrl(url)) {
        sendEnvelope(errorEnvelope(QStringLiteral("Could not open vosu settings")));
      }
    } catch (const std::exception &error) {
      sendEnvelope(errorEnvelope(QString::fromUtf8(error.what())));
    }
  }

  void openAvailable() {
    try {
      const QByteArray baseUrl = options_.vosuUrl.toUtf8();
      const QUrl url(rustStringToQString(
          available_url_for_qt(asRustStr(baseUrl))));
      if (!QDesktopServices::openUrl(url)) {
        sendEnvelope(
            errorEnvelope(QStringLiteral("Could not open available vosu counters")));
      }
    } catch (const std::exception &error) {
      sendEnvelope(errorEnvelope(QString::fromUtf8(error.what())));
    }
  }

  bool listenForIpc() {
    server_.setSocketOptions(QLocalServer::UserAccessOption);
    const QString name = socketName();
    if (server_.listen(name)) {
      return true;
    }

    QLocalSocket probe;
    probe.connectToServer(name, QIODevice::WriteOnly);
    if (probe.waitForConnected(150)) {
      return false;
    }
    QLocalServer::removeServer(name);
    return server_.listen(name);
  }

  void setupIpcServer() {
    ipcReady_ = listenForIpc();
    if (!ipcReady_) {
      return;
    }
    connect(&server_, &QLocalServer::newConnection, this, [this] {
      while (QLocalSocket *socket = server_.nextPendingConnection()) {
        socket->setParent(this);
        socket->setProperty("vosulay-buffer", QByteArray());
        auto *timeout = new QTimer(socket);
        timeout->setSingleShot(true);
        timeout->setInterval(2000);
        connect(timeout, &QTimer::timeout, socket, &QLocalSocket::abort);
        timeout->start();
        connect(socket, &QLocalSocket::readyRead, this,
                [this, socket, timeout] { readIpc(socket, timeout); });
        connect(socket, &QLocalSocket::disconnected, socket,
                &QLocalSocket::deleteLater);
      }
    });
  }

  void replyIpc(QLocalSocket *socket, bool accepted) {
    socket->write(accepted ? QByteArrayLiteral("ok\n")
                           : QByteArrayLiteral("error\n"));
    socket->disconnectFromServer();
  }

  void readIpc(QLocalSocket *socket, QTimer *timeout) {
    QByteArray buffer = socket->property("vosulay-buffer").toByteArray();
    const qsizetype room = kMaximumIpcBytes - buffer.size();
    if (room <= 0) {
      timeout->stop();
      replyIpc(socket, false);
      return;
    }
    buffer.append(socket->read(room));
    if (buffer.size() == kMaximumIpcBytes && !buffer.endsWith('\n')) {
      timeout->stop();
      replyIpc(socket, false);
      return;
    }
    const qsizetype newline = buffer.indexOf('\n');
    if (newline < 0) {
      socket->setProperty("vosulay-buffer", buffer);
      return;
    }
    timeout->stop();
    const QByteArray payload = buffer.first(newline);

    QJsonParseError error;
    const auto document = QJsonDocument::fromJson(payload, &error);
    if (error.error != QJsonParseError::NoError || !document.isObject()) {
      replyIpc(socket, false);
      return;
    }
    const auto command = document.object();
    const QString name = command.value(QStringLiteral("command")).toString();
    if (name == QStringLiteral("toggle-edit")) {
      setEditing(!editing_);
    } else if (name == QStringLiteral("toggle-overlay")) {
      setOverlayVisibility(!isVisible());
    } else if (name == QStringLiteral("quit")) {
      replyIpc(socket, true);
      QMetaObject::invokeMethod(qApp, &QApplication::quit,
                                Qt::QueuedConnection);
      return;
    } else if (name == QStringLiteral("geometry")) {
      if (!options_.attach) {
        replyIpc(socket, false);
        return;
      }
      const int width = command.value(QStringLiteral("width")).toInt();
      const int height = command.value(QStringLiteral("height")).toInt();
      if (width <= 0 || height <= 0) {
        replyIpc(socket, false);
        return;
      }
      queueOverlayGeometry(
          QRect(command.value(QStringLiteral("x")).toInt(),
                command.value(QStringLiteral("y")).toInt(), width, height));
    } else {
      replyIpc(socket, false);
      return;
    }
    replyIpc(socket, true);
  }

  ParsedOptions options_;
  QWebEngineView *view_ = nullptr;
  QWebEnginePage *page_ = nullptr;
  OverlaySchemeHandler *overlaySchemeHandler_ = nullptr;
  QWebChannel *channel_ = nullptr;
  ManagerBridge *bridge_ = nullptr;
  LayerShellQt::Window *layerWindow_ = nullptr;
  QScreen *refreshScreen_ = nullptr;
  QLocalServer server_;
  QSystemTrayIcon tray_;
  QAction *editAction_ = nullptr;
  QAction *visibilityAction_ = nullptr;
  QThread *fetchWorker_ = nullptr;
  QThread *saveWorker_ = nullptr;
  std::optional<SaveRequest> pendingSave_;
  std::optional<QRect> pendingGeometry_;
  std::optional<QRect> appliedGeometry_;
  std::optional<QString> managerDocumentId_;
  std::optional<QString> pendingInitializationDocumentId_;
  std::optional<QString> initializationAwaitingEditingAppliedDocumentId_;
  bool geometryApplyScheduled_ = false;
  quint64 fetchGeneration_ = 0;
  bool fetchPending_ = false;
  bool shuttingDown_ = false;
  bool editing_ = false;
  bool activeProfileHasOverlays_ = true;
  bool editingOffApplied_ = false;
  bool persistenceError_ = false;
  bool ipcReady_ = false;
};

QString monitorDescription() {
  QString text = QStringLiteral("Available monitors (%1):")
                     .arg(QApplication::screens().size());
  for (qsizetype index = 0; index < QApplication::screens().size(); ++index) {
    text += QStringLiteral("\n  Monitor %1: %2")
                .arg(index)
                .arg(QApplication::screens().at(index)->name());
  }
  return text;
}

QScreen *parseScreen(const QString &value, QString *error) {
  if (value.isEmpty()) {
    *error = monitorDescription();
    return nullptr;
  }
  bool numeric = false;
  const int index = value.toInt(&numeric);
  const auto screens = QApplication::screens();
  if (numeric && index >= 0 && index < screens.size()) {
    return screens.at(index);
  }
  if (!numeric) {
    for (QScreen *screen : screens) {
      if (screen->name() == value) {
        return screen;
      }
    }
  }
  *error = QStringLiteral("Invalid monitor %1\n%2").arg(value,
                                                               monitorDescription());
  return nullptr;
}

std::optional<QRect> parseGeometry(const QString &value) {
  const auto parts = value.split(',');
  if (parts.size() != 4) {
    return std::nullopt;
  }
  int values[4];
  for (int index = 0; index < 4; ++index) {
    bool ok = false;
    values[index] = parts.at(index).trimmed().toInt(&ok);
    if (!ok) {
      return std::nullopt;
    }
  }
  if (values[2] <= 0 || values[3] <= 0) {
    return std::nullopt;
  }
  return QRect(values[0], values[1], values[2], values[3]);
}

bool sendIpcCommand(const QJsonObject &command) {
  const QByteArray payload =
      QJsonDocument(command).toJson(QJsonDocument::Compact) + '\n';
  if (payload.size() > kMaximumIpcBytes) {
    return false;
  }
  QDeadlineTimer deadline(2000);
  QLocalSocket socket;
  socket.connectToServer(socketName(), QIODevice::ReadWrite);
  if (!socket.waitForConnected(deadline.remainingTime())) {
    return false;
  }
  if (socket.write(payload) != payload.size() ||
      !socket.waitForBytesWritten(deadline.remainingTime())) {
    return false;
  }

  QByteArray response;
  while (!response.contains('\n')) {
    const qsizetype room = kMaximumIpcBytes - response.size();
    if (room <= 0) {
      return false;
    }
    response.append(socket.read(room));
    if (response.contains('\n')) {
      break;
    }
    const qint64 remaining = deadline.remainingTime();
    if (remaining <= 0 || !socket.waitForReadyRead(remaining)) {
      return false;
    }
  }
  return response.first(response.indexOf('\n')).trimmed() ==
         QByteArrayLiteral("ok");
}

} // namespace

std::int32_t run_qt_application(rust::Str arguments_json) {
  const QByteArray serialized(arguments_json.data(),
                              static_cast<qsizetype>(arguments_json.size()));
  QJsonParseError jsonError;
  const auto json = QJsonDocument::fromJson(serialized, &jsonError);
  if (jsonError.error != QJsonParseError::NoError || !json.isArray()) {
    QTextStream(stderr) << "vosulay: invalid argument list\n";
    return 2;
  }

  QStringList arguments;
  for (const auto &value : json.array()) {
    if (!value.isString()) {
      QTextStream(stderr) << "vosulay: invalid argument list\n";
      return 2;
    }
    arguments.push_back(value.toString());
  }
  if (arguments.isEmpty()) {
    arguments.push_back(QStringLiteral("vosulay"));
  }

  std::vector<QByteArray> encodedArguments;
  encodedArguments.reserve(static_cast<std::size_t>(arguments.size()));
  for (const auto &argument : arguments) {
    encodedArguments.push_back(argument.toLocal8Bit());
  }
  std::vector<char *> argv;
  argv.reserve(encodedArguments.size());
  for (auto &argument : encodedArguments) {
    argv.push_back(argument.data());
  }
  int argc = static_cast<int>(argv.size());
  QCommandLineParser parser;
  parser.setApplicationDescription(
      QStringLiteral("Native vosu overlay manager"));
  const QCommandLineOption help = parser.addHelpOption();
  const QCommandLineOption version = parser.addVersionOption();
  const QCommandLineOption url(
      {QStringLiteral("t"), QStringLiteral("vosu-url")},
      QStringLiteral("Base URL of the vosu instance"),
      QStringLiteral("url"), QStringLiteral("http://127.0.0.1:24050"));
  const QCommandLineOption monitor(
      {QStringLiteral("m"), QStringLiteral("monitor")},
      QStringLiteral("Monitor index or name for vosulay"),
      QStringLiteral("monitor"));
  const QCommandLineOption fps(
      {QStringLiteral("f"), QStringLiteral("fps-limit")},
      QStringLiteral("Animation frame limit; use auto or 0 for screen refresh"),
      QStringLiteral("fps"));
  const QCommandLineOption attach(
      {QStringLiteral("a"), QStringLiteral("attach")},
      QStringLiteral("Attach vosulay to geometry received over IPC"));
  const QCommandLineOption edit(
      {QStringLiteral("e"), QStringLiteral("edit")},
      QStringLiteral("Toggle editing in the running vosulay"));
  const QCommandLineOption ipcOverlay(
      {QStringLiteral("i"), QStringLiteral("ipc-toggle-overlay")},
      QStringLiteral("Toggle visibility of the running vosulay"));
  const QCommandLineOption ipcQuit(
      {QStringLiteral("q"), QStringLiteral("ipc-quit")},
      QStringLiteral("Quit the running vosulay"));
  const QCommandLineOption ipcGeometry(
      QStringLiteral("ipc-geometry"),
      QStringLiteral("Set attached vosulay geometry as x,y,w,h"),
      QStringLiteral("x,y,w,h"));
  parser.addOptions(
      {url, monitor, fps, attach, edit, ipcOverlay, ipcQuit, ipcGeometry});

  if (!parser.parse(arguments)) {
    QTextStream(stderr) << "vosulay: " << parser.errorText() << '\n';
    return 2;
  }
  const bool immediate =
      parser.isSet(help) || parser.isSet(version) || parser.isSet(edit) ||
      parser.isSet(ipcOverlay) || parser.isSet(ipcQuit) ||
      parser.isSet(ipcGeometry);
  if (immediate) {
    QCoreApplication application(argc, argv.data());
    QCoreApplication::setApplicationName(QString::fromLatin1(kApplicationName));
    QCoreApplication::setApplicationVersion(
        QString::fromLatin1(kApplicationVersion));
    if (parser.isSet(help)) {
      QTextStream(stdout) << parser.helpText();
      return 0;
    }
    if (parser.isSet(version)) {
      QTextStream(stdout) << "vosulay " << kApplicationVersion << '\n';
      return 0;
    }

    if (parser.isSet(edit)) {
      return sendIpcCommand(
                 QJsonObject{{QStringLiteral("command"),
                              QStringLiteral("toggle-edit")}})
                 ? 0
                 : 1;
    }
    if (parser.isSet(ipcOverlay)) {
      return sendIpcCommand(
                 QJsonObject{{QStringLiteral("command"),
                              QStringLiteral("toggle-overlay")}})
                 ? 0
                 : 1;
    }
    if (parser.isSet(ipcQuit)) {
      return sendIpcCommand(QJsonObject{{QStringLiteral("command"),
                                        QStringLiteral("quit")}})
                 ? 0
                 : 1;
    }
    if (parser.isSet(ipcGeometry)) {
      const auto geometry = parseGeometry(parser.value(ipcGeometry));
      if (!geometry.has_value()) {
        QTextStream(stderr)
            << "vosulay: --ipc-geometry expects x,y,w,h with positive size\n";
        return 2;
      }
      return sendIpcCommand(
                 QJsonObject{{QStringLiteral("command"),
                              QStringLiteral("geometry")},
                             {QStringLiteral("x"), geometry->x()},
                             {QStringLiteral("y"), geometry->y()},
                             {QStringLiteral("width"), geometry->width()},
                             {QStringLiteral("height"), geometry->height()}})
                 ? 0
                 : 1;
    }
  }

  ParsedOptions options;
  options.vosuUrl = parser.value(url);
  options.attach = parser.isSet(attach);

  if (parser.isSet(fps)) {
    const QString value = parser.value(fps).trimmed();
    if (value.compare(QStringLiteral("auto"), Qt::CaseInsensitive) == 0 ||
        value == QStringLiteral("0")) {
      options.fpsLimit = 0;
    } else {
      bool ok = false;
      options.fpsLimit = value.toInt(&ok);
      if (!ok || options.fpsLimit <= 0) {
        QTextStream(stderr)
            << "vosulay: invalid fps limit; expected auto, 0, or a positive integer\n";
        return 2;
      }
    }
  }

#if QT_VERSION < QT_VERSION_CHECK(6, 5, 0)
  qputenv("QT_WAYLAND_SHELL_INTEGRATION", "layer-shell");
#endif
  configureChromium(arguments);
  registerOverlayScheme();

  QApplication application(argc, argv.data());
  QCoreApplication::setApplicationName(QString::fromLatin1(kApplicationName));
  QCoreApplication::setApplicationVersion(
      QString::fromLatin1(kApplicationVersion));
  QApplication::setApplicationDisplayName(QStringLiteral("vosulay"));
  QApplication::setQuitOnLastWindowClosed(false);

  options.screen = QApplication::primaryScreen();
  if (!options.attach && parser.isSet(monitor)) {
    QString error;
    options.screen = parseScreen(parser.value(monitor), &error);
    if (options.screen == nullptr) {
      QTextStream(stderr) << "vosulay: " << error << '\n';
      return 2;
    }
  }

  ShellWindow window(std::move(options));
  if (!window.ipcReady()) {
    QTextStream(stderr)
        << "vosulay: another instance is running or IPC could not be started\n";
    return 1;
  }
  window.initializeWindow();
  return application.exec();
}

} // namespace vosulay
