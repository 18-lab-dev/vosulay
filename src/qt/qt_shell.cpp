#include "qt_shell.h"

#include "bridge.h"
#include "vosulay/src/ffi.rs.h"

#include <LayerShellQt/Window>

#include <QAction>
#include <QApplication>
#include <QByteArray>
#include <QCoreApplication>
#include <QDesktopServices>
#include <QDeadlineTimer>
#include <QDir>
#include <QDebug>
#include <QFile>
#include <QDBusArgument>
#include <QDBusConnection>
#include <QDBusInterface>
#include <QDBusMetaType>
#include <QDBusObjectPath>
#include <QDBusPendingCallWatcher>
#include <QDBusPendingReply>
#include <QFileInfo>
#include <QEventLoop>
#include <QGuiApplication>
#include <QKeyEvent>
#include <QIcon>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QJsonParseError>
#include <QLocalServer>
#include <QLocalSocket>
#include <QMargins>
#include <QList>
#include <QUuid>
#include <QMimeDatabase>
#include <QMenu>
#include <QKeySequence>
#include <QPointer>
#include <QRegion>
#include <QResource>
#include <QScreen>
#include <QShortcut>
#include <QScopeGuard>
#include <QStandardPaths>
#include <QSaveFile>
#include <QStringList>
#include <QStyle>
#include <QSystemTrayIcon>
#include <QTextStream>
#include <QThread>
#include <QTimer>
#include <QUrl>
#include <QWheelEvent>
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

#include <cstdint>
#include <exception>
#include <functional>
#include <memory>
#include <optional>
#include <stdexcept>
#include <utility>
#include <vector>

#if defined(Q_OS_UNIX)
#include <unistd.h>
#endif

static void initializeVosulayResources() {
  Q_INIT_RESOURCE(vosulay);
}

namespace vosulay::portal {

struct FilterCondition {
  uint type;
  QString pattern;
};

using FilterConditions = QList<FilterCondition>;

struct Filter {
  QString name;
  FilterConditions conditions;
};

using Filters = QList<Filter>;

QDBusArgument &operator<<(QDBusArgument &argument,
                          const FilterCondition &condition) {
  argument.beginStructure();
  argument << condition.type << condition.pattern;
  argument.endStructure();
  return argument;
}

const QDBusArgument &operator>>(const QDBusArgument &argument,
                                FilterCondition &condition) {
  argument.beginStructure();
  argument >> condition.type >> condition.pattern;
  argument.endStructure();
  return argument;
}

QDBusArgument &operator<<(QDBusArgument &argument, const Filter &filter) {
  argument.beginStructure();
  argument << filter.name << filter.conditions;
  argument.endStructure();
  return argument;
}

const QDBusArgument &operator>>(const QDBusArgument &argument, Filter &filter) {
  argument.beginStructure();
  argument >> filter.name >> filter.conditions;
  argument.endStructure();
  return argument;
}

} // namespace vosulay::portal

Q_DECLARE_METATYPE(vosulay::portal::FilterCondition)
Q_DECLARE_METATYPE(vosulay::portal::FilterConditions)
Q_DECLARE_METATYPE(vosulay::portal::Filter)
Q_DECLARE_METATYPE(vosulay::portal::Filters)

namespace vosulay {

PortalResponseReceiver::PortalResponseReceiver(
    std::function<void(uint, const QVariantMap &)> callback, QObject *parent)
    : QObject(parent), callback_(std::move(callback)) {}

void PortalResponseReceiver::response(uint response,
                                      const QVariantMap &results) {
  callback_(response, results);
}

ManagerBridge::ManagerBridge(std::function<void(const QString &)> handler,
                             QObject *parent)
    : QObject(parent), handler_(std::move(handler)) {}

void ManagerBridge::postMessage(const QString &message) { handler_(message); }

namespace {

constexpr auto kApplicationName = "vosulay";
constexpr auto kApplicationVersion = "0.1.0";
constexpr auto kOverlayScheme = "vosulay-overlay";
constexpr qsizetype kMaximumIpcBytes = 4096;
constexpr qsizetype kMaximumProfileBytes = 1024 * 1024;

QIcon loadApplicationIcon() {
  QIcon icon(QStringLiteral(":/vosulay/vosulay-app-icon.png"));
  if (!icon.isNull()) {
    return icon;
  }

  icon = QIcon::fromTheme(QStringLiteral("applications-games"));
  if (!icon.isNull()) {
    return icon;
  }
  return QApplication::style()->standardIcon(QStyle::SP_ComputerIcon);
}
QString rustStringToQString(const rust::String &value) {
  return QString::fromUtf8(value.data(), static_cast<qsizetype>(value.size()));
}

rust::Str asRustStr(const QByteArray &value) {
  return rust::Str(value.constData(), static_cast<std::size_t>(value.size()));
}



double detectedRefreshRate(QScreen *screen) {
  constexpr double fallback = 60.0;
  if (screen == nullptr || !qIsFinite(screen->refreshRate()) ||
      screen->refreshRate() < 1.0) {
    return fallback;
  }
  return qBound(30.0, screen->refreshRate(), 360.0);
}


void installScript(
    QWebEnginePage *page, const QString &name, const QString &source,
    bool subframes,
    QWebEngineScript::ScriptWorldId world = QWebEngineScript::MainWorld) {
  QWebEngineScript script;
  script.setName(name);
  script.setInjectionPoint(QWebEngineScript::DocumentCreation);
  script.setRunsOnSubFrames(subframes);
  script.setWorldId(world);
  script.setSourceCode(source);
  page->scripts().insert(script);
}

void updateFrameRate(QWebEngineFrame frame, const QString &source) {
  frame.runJavaScript(source);
  for (const auto &child : frame.children()) {
    updateFrameRate(child, source);
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

class OverlaySchemeHandler final : public QWebEngineUrlSchemeHandler {
public:
  explicit OverlaySchemeHandler(QObject *parent)
      : QWebEngineUrlSchemeHandler(parent),
        authorization_(new_overlay_authorization()) {}

  bool authorizeUrl(const QUrl &url) {
    const QByteArray encoded = url.toEncoded(QUrl::FullyEncoded);
    return authorize_overlay_url(*authorization_, asRustStr(encoded));
  }

  bool authorizeUrls(const QJsonArray &urls) {
    const QByteArray encoded =
        QJsonDocument(urls).toJson(QJsonDocument::Compact);
    return authorize_overlay_urls(*authorization_, asRustStr(encoded));
  }

  void requestStarted(QWebEngineUrlRequestJob *job) override {
    const QByteArray encoded =
        job->requestUrl().toEncoded(QUrl::FullyEncoded);
    const OverlayRequest request =
        resolve_overlay_url(*authorization_, asRustStr(encoded));
    switch (request.status) {
    case OverlayRequestStatus::Invalid:
      job->fail(QWebEngineUrlRequestJob::UrlInvalid);
      return;
    case OverlayRequestStatus::Denied:
      job->fail(QWebEngineUrlRequestJob::RequestDenied);
      return;
    case OverlayRequestStatus::NotFound:
      job->fail(QWebEngineUrlRequestJob::UrlNotFound);
      return;
    case OverlayRequestStatus::Ready:
      break;
    }

    const QString path = rustStringToQString(request.path);
    const QFileInfo targetInfo(path);
    auto *file = new QFile(path, job);
    if (!file->open(QIODevice::ReadOnly)) {
      job->fail(QWebEngineUrlRequestJob::RequestFailed);
      return;
    }
    const QByteArray mimeType =
        QMimeDatabase().mimeTypeForFile(targetInfo).name().toUtf8();
    job->reply(mimeType, file);
  }

private:
  rust::Box<OverlayAuthorization> authorization_;
};

void configureQtWebEngineEnvironment() {
  if (qEnvironmentVariableIsEmpty("QTWEBENGINEPROCESS_PATH")) {
    qputenv("QTWEBENGINEPROCESS_PATH", VOSULAY_QTWEBENGINEPROCESS_PATH);
  }
  if (qEnvironmentVariableIsEmpty("QTWEBENGINE_RESOURCES_PATH")) {
    qputenv("QTWEBENGINE_RESOURCES_PATH",
            VOSULAY_QTWEBENGINE_RESOURCES_PATH);
  }
  if (qEnvironmentVariableIsEmpty("QTWEBENGINE_LOCALES_PATH")) {
    qputenv("QTWEBENGINE_LOCALES_PATH", VOSULAY_QTWEBENGINE_LOCALES_PATH);
  }
}

void registerOverlayScheme() {
  QWebEngineUrlScheme scheme{QByteArray(kOverlayScheme)};
  scheme.setSyntax(QWebEngineUrlScheme::Syntax::HostAndPort);
  scheme.setDefaultPort(443);
  scheme.setFlags(QWebEngineUrlScheme::SecureScheme |
                  QWebEngineUrlScheme::CorsEnabled |
                  QWebEngineUrlScheme::FetchApiAllowed);
  QWebEngineUrlScheme::registerScheme(scheme);
}

class ZoomLockedWebEngineView final : public QWebEngineView {
public:
  explicit ZoomLockedWebEngineView(QWidget *parent = nullptr)
      : QWebEngineView(parent) {
    qApp->installEventFilter(this);
  }

  ~ZoomLockedWebEngineView() override { qApp->removeEventFilter(this); }

protected:
  bool eventFilter(QObject *watched, QEvent *event) override {
    if (belongsToView(watched) && isZoomGesture(event)) {
      event->accept();
      return true;
    }
    return QWebEngineView::eventFilter(watched, event);
  }

private:
  bool belongsToView(const QObject *object) const {
    for (auto *current = object; current != nullptr;
         current = current->parent()) {
      if (current == this)
        return true;
    }
    return false;
  }

  static bool isZoomGesture(const QEvent *event) {
    if (event->type() == QEvent::Wheel) {
      const auto *wheelEvent = static_cast<const QWheelEvent *>(event);
      return wheelEvent->modifiers().testFlag(Qt::ControlModifier);
    }

    if (event->type() != QEvent::KeyPress &&
        event->type() != QEvent::ShortcutOverride) {
      return false;
    }

    const auto *keyEvent = static_cast<const QKeyEvent *>(event);
    const Qt::KeyboardModifiers modifiers = keyEvent->modifiers();
    if (!modifiers.testFlag(Qt::ControlModifier) ||
        modifiers.testFlag(Qt::AltModifier) ||
        modifiers.testFlag(Qt::MetaModifier)) {
      return false;
    }

    return keyEvent->key() == Qt::Key_Plus ||
           keyEvent->key() == Qt::Key_Equal ||
           keyEvent->key() == Qt::Key_Minus;
  }
};

class PromptAwareWebEnginePage final : public QWebEnginePage {
public:
  using PromptCallback = std::function<void()>;

  PromptAwareWebEnginePage(PromptCallback onPromptOpened,
                           PromptCallback onPromptClosed, QObject *parent)
      : QWebEnginePage(parent), onPromptOpened_(std::move(onPromptOpened)),
        onPromptClosed_(std::move(onPromptClosed)) {}

private:
  template <typename Function>
  decltype(auto) whilePromptOpen(Function &&function) {
    onPromptOpened_();
    const auto promptGuard =
        qScopeGuard([this] { onPromptClosed_(); });
    return std::forward<Function>(function)();
  }

  PromptCallback onPromptOpened_;
  PromptCallback onPromptClosed_;

protected:
  QStringList
  chooseFiles(FileSelectionMode mode, const QStringList &oldFiles,
              const QStringList &acceptedMimeTypes) override {
    return whilePromptOpen([&] {
      return QWebEnginePage::chooseFiles(mode, oldFiles, acceptedMimeTypes);
    });
  }

  void javaScriptAlert(const QUrl &securityOrigin,
                       const QString &message) override {
    whilePromptOpen([&] {
      QWebEnginePage::javaScriptAlert(securityOrigin, message);
    });
  }

  bool javaScriptConfirm(const QUrl &securityOrigin,
                         const QString &message) override {
    return whilePromptOpen([&] {
      return QWebEnginePage::javaScriptConfirm(securityOrigin, message);
    });
  }

  bool javaScriptPrompt(const QUrl &securityOrigin, const QString &message,
                        const QString &defaultValue, QString *result) override {
    return whilePromptOpen([&] {
      return QWebEnginePage::javaScriptPrompt(securityOrigin, message,
                                              defaultValue, result);
    });
  }

};

class ShellWindow final : public QWidget {
public:
  explicit ShellWindow(const QtShellOptions &options, QScreen *screen,
                       const QIcon &applicationIcon)
      : vosuUrl_(rustStringToQString(options.vosu_url)),
        fpsLimit_(options.fps_limit), attach_(options.attach), screen_(screen),
        server_(this), tray_(this) {
    setWindowIcon(applicationIcon);
    setAttribute(Qt::WA_TranslucentBackground);
    setWindowFlags(Qt::FramelessWindowHint | Qt::NoDropShadowWindowHint |
                   Qt::WindowStaysOnTopHint | Qt::X11BypassWindowManagerHint);

    auto *layout = new QVBoxLayout(this);
    layout->setContentsMargins(0, 0, 0, 0);
    view_ = new ZoomLockedWebEngineView(this);
    view_->setAttribute(Qt::WA_TranslucentBackground);
    view_->setContextMenuPolicy(Qt::NoContextMenu);
    layout->addWidget(view_);

    auto *interactionShortcut =
        new QShortcut(QKeySequence(Qt::CTRL | Qt::Key_I), this);
    interactionShortcut->setContext(Qt::WindowShortcut);
    connect(interactionShortcut, &QShortcut::activated, this, [this] {
      if (editing_) {
        sendEnvelope(QJsonObject{{QStringLiteral("type"),
                                  QStringLiteral("toggle-interaction")}});
      }
    });

    page_ = new PromptAwareWebEnginePage(
        [this] { suspendForPrompt(); },
        [this] { restoreAfterPrompt(); }, view_);
    page_->setBackgroundColor(Qt::transparent);
    view_->setPage(page_);
    overlaySchemeHandler_ = new OverlaySchemeHandler(page_->profile());
    page_->profile()->installUrlSchemeHandler(QByteArray(kOverlayScheme),
                                              overlaySchemeHandler_);
    page_->settings()->setAttribute(
        QWebEngineSettings::LocalContentCanAccessFileUrls, false);
    page_->settings()->setAttribute(
        QWebEngineSettings::LocalContentCanAccessRemoteUrls, true);

    const QByteArray vosuUrl = vosuUrl_.toUtf8();
    installScript(page_, QStringLiteral("vosulay-counter-metadata"),
                  rustStringToQString(
                      counter_metadata_script_for_qt(asRustStr(vosuUrl))),
                  true);

    if (fpsLimit_ >= 0) {
      installLimiter();
    }
    channel_ = new QWebChannel(page_);
    bridge_ = new ManagerBridge(
        [this](const QString &message) { handleManagerMessage(message); },
        channel_);
    channel_->registerObject(QStringLiteral("vosulayBridge"), bridge_);
    page_->setWebChannel(channel_, QWebEngineScript::ApplicationWorld);

    QFile webChannelSource(QStringLiteral(":/qtwebchannel/qwebchannel.js"));
    if (!webChannelSource.open(QIODevice::ReadOnly)) {
      qFatal("Could not load Qt WebChannel JavaScript");
    }
    QString isolatedBridge =
        QString::fromUtf8(webChannelSource.readAll()) + QLatin1Char('\n') +
        rustStringToQString(manager_bridge_script_for_qt());
    installScript(page_, QStringLiteral("vosulay-manager-bridge"),
                  isolatedBridge, false, QWebEngineScript::ApplicationWorld);

    connect(windowHandleOrCreate(), &QWindow::screenChanged, this,
            [this](QScreen *screen) {
              screen_ = screen;
              connectRefreshRateSignal(screen);
              syncAutoFrameRate();
            });
    connectRefreshRateSignal(screen_);

    setupTray(applicationIcon);
    setupIpcServer();

    const auto html = rustStringToQString(
        manager_document_for_qt(static_cast<std::int32_t>(fpsLimit_)));
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
    QScreen *screen = screen_ != nullptr ? screen_
                                                 : QApplication::primaryScreen();
    screen_ = screen;
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

  void setupTray(const QIcon &icon) {
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
    if (!visible) {
      applyInputCapture(false);
    }
    setVisible(visible);
    if (visible) {
      applyInputCapture(editing_ && promptDepth_ == 0);
    }
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
  void applyInputCapture(bool capture) {
    QWindow *window = windowHandleOrCreate();
    if (QApplication::platformName() == QStringLiteral("xcb")) {
      window->setFlag(Qt::WindowTransparentForInput, !capture);
    } else if (capture) {
      window->setMask(QRegion());
    } else {
      window->setMask(QRegion(-1, -1, 1, 1));
    }

    if (layerWindow_ != nullptr) {
      layerWindow_->setKeyboardInteractivity(
          capture ? LayerShellQt::Window::KeyboardInteractivityExclusive
                  : LayerShellQt::Window::KeyboardInteractivityNone);
    }
  }

  void suspendForPrompt() {
    if (promptDepth_++ != 0) {
      return;
    }
    promptOverlayWasVisible_ = view_->isVisible();
    view_->hide();
    applyInputCapture(false);
    QGuiApplication::sync();
  }

  void restoreAfterPrompt() {
    if (promptDepth_ == 0 || --promptDepth_ != 0) {
      return;
    }
    view_->setVisible(promptOverlayWasVisible_);
    applyInputCapture(editing_ && isVisible());
  }


  void setEditing(bool editing) {
    editing_ = editing;
    if (editing) {
      updatePageLifecycle();
    } else {
      editingOffApplied_ = false;
    }
    applyInputCapture(editing_ && promptDepth_ == 0 && isVisible());
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
      screen = screen_;
    }
    if (screen != nullptr && screen != screen_) {
      screen_ = screen;
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
    const double nativeRefreshRate = detectedRefreshRate(screen_);
    const double fps = fpsLimit_ == 0
                           ? nativeRefreshRate
                           : static_cast<double>(fpsLimit_);
    installScript(
        page_, QStringLiteral("vosulay-animation-frame-limiter"),
        rustStringToQString(
            limiter_script_for_qt(fps, nativeRefreshRate)),
        true);
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
    if (fpsLimit_ != 0 || page_ == nullptr) {
      return;
    }
    const double fps = detectedRefreshRate(screen_);
    const auto oldScripts =
        page_->scripts().find(QStringLiteral("vosulay-animation-frame-limiter"));
    for (const auto &oldScript : oldScripts) {
      page_->scripts().remove(oldScript);
    }
    installScript(page_, QStringLiteral("vosulay-animation-frame-limiter"),
                  rustStringToQString(limiter_script_for_qt(fps, fps)), true);
    updateFrameRate(page_->mainFrame(),
                    rustStringToQString(frame_rate_script_for_qt(fps)));
  }

  void sendEnvelope(const QJsonObject &envelope) {
    const QByteArray payload =
        QJsonDocument(envelope).toJson(QJsonDocument::Compact);
    page_->runJavaScript(rustStringToQString(
        manager_receive_script_for_qt(asRustStr(payload))));
  }

  void handleManagerMessage(const QString &message) {
    const QByteArray encoded = message.toUtf8();
    const ManagerMessage request =
        parse_manager_message_for_qt(asRustStr(encoded));
    const auto payload = [&request] {
      const QByteArray json = rustStringToQString(request.payload_json).toUtf8();
      return QJsonDocument::fromJson(json).object();
    };

    switch (request.kind) {
    case ManagerMessageKind::Invalid:
      sendEnvelope(errorEnvelope(rustStringToQString(request.error)));
      return;
    case ManagerMessageKind::Ready: {
      const QString documentId = rustStringToQString(request.document_id);
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
      return;
    }
    case ManagerMessageKind::SaveConfig:
      saveConfiguration(payload(), rustStringToQString(request.save_id));
      return;
    case ManagerMessageKind::RefreshCounters:
      fetchCounters();
      return;
    case ManagerMessageKind::EditingApplied:
      if (!request.editing && !editing_) {
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
      return;
    case ManagerMessageKind::SetEditing:
      setEditing(request.editing);
      return;
    case ManagerMessageKind::ChooseOverlayFile:
      chooseOverlayFile();
      return;
    case ManagerMessageKind::ExportProfile:
      exportProfile(payload());
      return;
    case ManagerMessageKind::ImportProfile:
      importProfile();
      return;
    case ManagerMessageKind::AuthorizeProfileImport:
      authorizeProfileImport(payload());
      return;
    case ManagerMessageKind::OpenSettings:
      openSettings(rustStringToQString(request.counter_id));
      return;
    case ManagerMessageKind::OpenAvailable:
      openAvailable();
      return;
    }
  }

  QString choosePortalFile(const QString &method, const QString &title,
                           const portal::Filters &filters,
                           const QString &currentName = {}) {
    suspendForPrompt();

    bool restored = false;
    const auto restoreOverlay = [&] {
      if (restored) {
        return;
      }
      restored = true;
      restoreAfterPrompt();
    };
    const auto restorationGuard = qScopeGuard(restoreOverlay);

    static const bool portalTypesRegistered = [] {
      qDBusRegisterMetaType<portal::FilterCondition>();
      qDBusRegisterMetaType<portal::FilterConditions>();
      qDBusRegisterMetaType<portal::Filter>();
      qDBusRegisterMetaType<portal::Filters>();
      return true;
    }();
    Q_UNUSED(portalTypesRegistered);

    constexpr auto portalService = "org.freedesktop.portal.Desktop";
    constexpr auto portalPath = "/org/freedesktop/portal/desktop";
    constexpr auto fileChooserInterface =
        "org.freedesktop.portal.FileChooser";
    constexpr auto requestInterface = "org.freedesktop.portal.Request";

    QDBusConnection bus = QDBusConnection::sessionBus();
    QString senderName = bus.baseService();
    senderName.remove(QLatin1Char(':'));
    senderName.replace(QLatin1Char('.'), QLatin1Char('_'));
    if (!bus.isConnected() || senderName.isEmpty()) {
      restoreOverlay();
      sendEnvelope(errorEnvelope(
          QStringLiteral("Could not connect to the desktop portal")));
      return {};
    }

    const QString handleToken =
        QUuid::createUuid().toString(QUuid::Id128);
    const QString predictedRequestPath =
        QStringLiteral("/org/freedesktop/portal/desktop/request/%1/%2")
            .arg(senderName, handleToken);

    QEventLoop responseLoop;
    bool responseFinished = false;
    uint responseCode = 2;
    QVariantMap responseResults;
    QString portalError;
    PortalResponseReceiver receiver(
        [&](uint code, const QVariantMap &results) {
          if (responseFinished) {
            return;
          }
          responseFinished = true;
          responseCode = code;
          responseResults = results;
          responseLoop.quit();
        });

    QStringList subscribedPaths;
    const auto subscribe = [&](const QString &requestPath) {
      if (!bus.connect(QString::fromLatin1(portalService), requestPath,
                       QString::fromLatin1(requestInterface),
                       QStringLiteral("Response"), &receiver,
                       SLOT(response(uint,QVariantMap)))) {
        return false;
      }
      subscribedPaths.append(requestPath);
      return true;
    };
    const auto unsubscribe = [&](const QString &requestPath) {
      bus.disconnect(QString::fromLatin1(portalService), requestPath,
                     QString::fromLatin1(requestInterface),
                     QStringLiteral("Response"), &receiver,
                     SLOT(response(uint,QVariantMap)));
      subscribedPaths.removeAll(requestPath);
    };
    const auto connectionGuard = qScopeGuard([&] {
      const QStringList paths = subscribedPaths;
      for (const QString &requestPath : paths) {
        unsubscribe(requestPath);
      }
    });

    if (!subscribe(predictedRequestPath)) {
      restoreOverlay();
      sendEnvelope(errorEnvelope(
          QStringLiteral("Could not monitor the desktop file chooser")));
      return {};
    }

    QVariantMap options;
    options.insert(QStringLiteral("handle_token"), handleToken);
    options.insert(QStringLiteral("modal"), true);
    options.insert(QStringLiteral("filters"), QVariant::fromValue(filters));
    if (method == QStringLiteral("OpenFile")) {
      options.insert(QStringLiteral("multiple"), false);
    }
    if (!currentName.isEmpty()) {
      options.insert(QStringLiteral("current_name"), currentName);
    }

    QDBusInterface portal(QString::fromLatin1(portalService),
                          QString::fromLatin1(portalPath),
                          QString::fromLatin1(fileChooserInterface), bus);
    if (!portal.isValid()) {
      restoreOverlay();
      sendEnvelope(errorEnvelope(
          QStringLiteral("Desktop file chooser is unavailable")));
      return {};
    }

    QDBusPendingCallWatcher portalCall(
        portal.asyncCall(method, QString(), title, options));
    QObject::connect(
        &portalCall, &QDBusPendingCallWatcher::finished, &receiver,
        [&](QDBusPendingCallWatcher *watcher) {
          if (responseFinished) {
            return;
          }
          const QDBusPendingReply<QDBusObjectPath> reply = *watcher;
          if (reply.isError()) {
            portalError = reply.error().message();
            if (portalError.isEmpty()) {
              portalError =
                  QStringLiteral("Desktop file chooser request failed");
            }
            responseFinished = true;
            responseLoop.quit();
            return;
          }

          const QString returnedRequestPath = reply.value().path();
          if (returnedRequestPath.isEmpty()) {
            portalError =
                QStringLiteral("Desktop file chooser returned an invalid request");
            responseFinished = true;
            responseLoop.quit();
            return;
          }
          if (returnedRequestPath != predictedRequestPath) {
            if (!subscribe(returnedRequestPath)) {
              portalError =
                  QStringLiteral("Could not monitor the desktop file chooser");
              responseFinished = true;
              responseLoop.quit();
              return;
            }
            unsubscribe(predictedRequestPath);
          }
        });

    if (!responseFinished) {
      responseLoop.exec();
    }
    restoreOverlay();

    if (!portalError.isEmpty()) {
      sendEnvelope(errorEnvelope(portalError));
      return {};
    }
    if (responseCode == 1) {
      return {};
    }
    if (responseCode != 0) {
      sendEnvelope(
          errorEnvelope(QStringLiteral("Desktop file chooser reported an error")));
      return {};
    }

    const QStringList uris =
        responseResults.value(QStringLiteral("uris")).toStringList();
    if (uris.isEmpty()) {
      sendEnvelope(errorEnvelope(
          QStringLiteral("Desktop file chooser returned no file")));
      return {};
    }
    const QUrl chosenUri(uris.constFirst());
    if (!chosenUri.isValid() || !chosenUri.isLocalFile()) {
      sendEnvelope(errorEnvelope(
          QStringLiteral("Desktop file chooser returned a non-local file")));
      return {};
    }
    const QString path = chosenUri.toLocalFile();
    if (path.isEmpty()) {
      sendEnvelope(errorEnvelope(
          QStringLiteral("Desktop file chooser returned an invalid file")));
      return {};
    }
    return path;
  }

  void chooseOverlayFile() {
    const portal::Filters filters{
        {QStringLiteral("HTML files"),
         {{0, QStringLiteral("*.html")}, {0, QStringLiteral("*.htm")}}}};
    const QString path =
        choosePortalFile(QStringLiteral("OpenFile"),
                         QStringLiteral("Choose HTML overlay"), filters);
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
      sendEnvelope(errorEnvelope(
          QStringLiteral("Selected overlay is not a readable HTML file")));
      return;
    }

    const QByteArray pathBytes = fileInfo.absoluteFilePath().toUtf8();
    const QByteArray token =
        QUuid::createUuid()
            .toString(QUuid::WithoutBraces)
            .remove(QLatin1Char('-'))
            .toUtf8();
    QUrl selectedUrl;
    try {
      selectedUrl = QUrl(rustStringToQString(
          overlay_url_for_qt(asRustStr(pathBytes), asRustStr(token))));
    } catch (const std::exception &) {
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

  void exportProfile(const QJsonValue &profileValue) {
    const QJsonObject profile = profileValue.toObject();
    const QByteArray payload =
        QJsonDocument(profile).toJson(QJsonDocument::Compact);
    if (payload.size() > kMaximumProfileBytes) {
      sendEnvelope(errorEnvelope(
          QStringLiteral("The profile is too large to export (maximum 1 MiB)")));
      return;
    }

    const portal::Filters filters{
        {QStringLiteral("JSON profile files"),
         {{0, QStringLiteral("*.json")}}}};
    const QString filename = rustStringToQString(
        profile_filename_for_qt(asRustStr(payload)));
    const QString path =
        choosePortalFile(QStringLiteral("SaveFile"),
                         QStringLiteral("Export profile"), filters, filename);
    if (path.isEmpty()) {
      return;
    }
    const QFileInfo fileInfo(path);
    if (fileInfo.suffix().compare(QStringLiteral("json"),
                                  Qt::CaseInsensitive) != 0) {
      sendEnvelope(errorEnvelope(
          QStringLiteral("The export filename must end with .json")));
      return;
    }

    QSaveFile file(path);
    if (!file.open(QIODevice::WriteOnly)) {
      sendEnvelope(errorEnvelope(
          QStringLiteral("Could not open the selected file for export")));
      return;
    }
    if (file.write(payload) != payload.size()) {
      file.cancelWriting();
      sendEnvelope(errorEnvelope(
          QStringLiteral("Could not write the exported profile")));
      return;
    }
    if (!file.commit()) {
      sendEnvelope(errorEnvelope(
          QStringLiteral("Could not finish writing the exported profile")));
      return;
    }
    sendEnvelope(QJsonObject{
        {QStringLiteral("type"), QStringLiteral("profile-exported")}});
  }

  void importProfile() {
    const portal::Filters filters{
        {QStringLiteral("JSON profile files"),
         {{0, QStringLiteral("*.json")}}}};
    const QString path =
        choosePortalFile(QStringLiteral("OpenFile"),
                         QStringLiteral("Import profile"), filters);
    if (path.isEmpty()) {
      return;
    }

    const QFileInfo fileInfo(path);
    if (!fileInfo.isFile() || !fileInfo.isReadable() ||
        fileInfo.suffix().compare(QStringLiteral("json"),
                                  Qt::CaseInsensitive) != 0) {
      sendEnvelope(errorEnvelope(
          QStringLiteral("Selected profile is not a readable JSON file")));
      return;
    }
    if (fileInfo.size() > kMaximumProfileBytes) {
      sendEnvelope(errorEnvelope(
          QStringLiteral("Selected profile is too large (maximum 1 MiB)")));
      return;
    }

    QFile file(fileInfo.absoluteFilePath());
    if (!file.open(QIODevice::ReadOnly)) {
      sendEnvelope(errorEnvelope(
          QStringLiteral("Selected profile could not be opened")));
      return;
    }
    const QByteArray payload = file.read(kMaximumProfileBytes + 1);
    if (file.error() != QFileDevice::NoError) {
      sendEnvelope(errorEnvelope(
          QStringLiteral("Selected profile could not be read")));
      return;
    }
    if (payload.size() > kMaximumProfileBytes) {
      sendEnvelope(errorEnvelope(
          QStringLiteral("Selected profile is too large (maximum 1 MiB)")));
      return;
    }

    QJsonParseError parseError;
    const QJsonDocument document =
        QJsonDocument::fromJson(payload, &parseError);
    if (parseError.error != QJsonParseError::NoError ||
        !document.isObject()) {
      sendEnvelope(errorEnvelope(
          QStringLiteral("Selected profile is not a valid JSON object")));
      return;
    }

    const QJsonObject profile = document.object();

    sendEnvelope(
        QJsonObject{{QStringLiteral("type"), QStringLiteral("profile-import")},
                    {QStringLiteral("profile"), profile}});
  }

  void authorizeProfileImport(const QJsonValue &profileValue) {
    const QJsonObject profile = profileValue.toObject();
    const QJsonArray overlays =
        profile.value(QStringLiteral("overlays")).toArray();
    QJsonArray localOverlayUrls;
    for (const QJsonValue &overlayValue : overlays) {
      const QString url =
          overlayValue.toObject().value(QStringLiteral("url")).toString();
      if (QUrl(url).scheme() == QString::fromLatin1(kOverlayScheme)) {
        localOverlayUrls.append(url);
      }
    }

    if (!overlaySchemeHandler_->authorizeUrls(localOverlayUrls)) {
      sendEnvelope(errorEnvelope(QStringLiteral(
          "Imported profile contains a local overlay that could not be authorized")));
      return;
    }

    sendEnvelope(QJsonObject{
        {QStringLiteral("type"),
         QStringLiteral("profile-import-authorized")},
        {QStringLiteral("profile"), profileValue}});
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
      const QByteArray configJson =
          QJsonDocument(config.toObject()).toJson(QJsonDocument::Compact);
      activeProfileHasOverlays_ =
          config_has_active_overlays_for_qt(asRustStr(configJson));
      QJsonArray localOverlayUrls;
      const QJsonArray profiles =
          config.toObject().value(QStringLiteral("profiles")).toArray();
      for (const QJsonValue &profile : profiles) {
        const QJsonArray overlays =
            profile.toObject().value(QStringLiteral("overlays")).toArray();
        for (const QJsonValue &overlay : overlays) {
          const QString url =
              overlay.toObject().value(QStringLiteral("url")).toString();
          if (QUrl(url).scheme() == QString::fromLatin1(kOverlayScheme)) {
            localOverlayUrls.append(url);
          }
        }
      }
      overlaySchemeHandler_->authorizeUrls(localOverlayUrls);
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
    const QByteArray json =
        QJsonDocument(config.toObject()).toJson(QJsonDocument::Compact);
    SaveRequest save{json, std::move(saveId),
                     config_has_active_overlays_for_qt(asRustStr(json))};
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
    const QByteArray baseUrl = vosuUrl_.toUtf8();
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
      const QByteArray baseUrl = vosuUrl_.toUtf8();
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
      const QByteArray baseUrl = vosuUrl_.toUtf8();
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

    const IpcCommand command =
        parse_ipc_command_for_qt(asRustStr(payload), attach_);
    switch (command.kind) {
    case IpcCommandKind::Invalid:
      replyIpc(socket, false);
      return;
    case IpcCommandKind::ToggleEdit:
      setEditing(!editing_);
      break;
    case IpcCommandKind::ToggleOverlay:
      setOverlayVisibility(!isVisible());
      break;
    case IpcCommandKind::Quit:
      replyIpc(socket, true);
      QMetaObject::invokeMethod(qApp, &QApplication::quit,
                                Qt::QueuedConnection);
      return;
    case IpcCommandKind::Geometry:
      queueOverlayGeometry(
          QRect(command.x, command.y, command.width, command.height));
      break;
    }
    replyIpc(socket, true);
  }

  QString vosuUrl_;
  int fpsLimit_;
  bool attach_;
  QScreen *screen_;
  ZoomLockedWebEngineView *view_ = nullptr;
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
  int promptDepth_ = 0;
  bool promptOverlayWasVisible_ = true;
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


bool sendIpcCommand(QByteArray payload) {
  if (payload.contains('\n') || payload.size() + 1 > kMaximumIpcBytes) {
    return false;
  }
  payload.append('\n');
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

std::int32_t run_qt_application(rust::Str arguments_json,
                                const QtShellOptions &options) {
  const QByteArray serialized(arguments_json.data(),
                              static_cast<qsizetype>(arguments_json.size()));
  const QJsonDocument json = QJsonDocument::fromJson(serialized);
  if (!json.isArray()) {
    return 2;
  }

  std::vector<QByteArray> encodedArguments;
  encodedArguments.reserve(static_cast<std::size_t>(json.array().size()));
  for (const QJsonValue &value : json.array()) {
    if (!value.isString()) {
      return 2;
    }
    encodedArguments.push_back(value.toString().toLocal8Bit());
  }
  if (encodedArguments.empty()) {
    encodedArguments.emplace_back("vosulay");
  }

  std::vector<char *> argv;
  argv.reserve(encodedArguments.size());
  for (QByteArray &argument : encodedArguments) {
    argv.push_back(argument.data());
  }
  int argc = static_cast<int>(argv.size());

  const QByteArray chromiumFlags =
      rustStringToQString(options.chromium_flags).toUtf8();
  qputenv("QTWEBENGINE_CHROMIUM_FLAGS", chromiumFlags);

  const QByteArray ipcCommand =
      rustStringToQString(options.ipc_command).toUtf8();
  if (!ipcCommand.isEmpty()) {
    QCoreApplication application(argc, argv.data());
    QCoreApplication::setApplicationName(QString::fromLatin1(kApplicationName));
    QCoreApplication::setApplicationVersion(
        QString::fromLatin1(kApplicationVersion));
    return sendIpcCommand(ipcCommand) ? 0 : 1;
  }
  configureQtWebEngineEnvironment();

#if QT_VERSION < QT_VERSION_CHECK(6, 5, 0)
  qputenv("QT_WAYLAND_SHELL_INTEGRATION", "layer-shell");
#endif
  registerOverlayScheme();

  QApplication application(argc, argv.data());
  initializeVosulayResources();
  const QIcon icon = loadApplicationIcon();
  QApplication::setWindowIcon(icon);
  QCoreApplication::setApplicationName(QString::fromLatin1(kApplicationName));
  QCoreApplication::setApplicationVersion(
      QString::fromLatin1(kApplicationVersion));
  QApplication::setApplicationDisplayName(QStringLiteral("vosulay"));
  QApplication::setQuitOnLastWindowClosed(false);

  QScreen *screen = QApplication::primaryScreen();
  const QString monitor = rustStringToQString(options.monitor);
  if (!options.attach && !monitor.isEmpty()) {
    QString error;
    screen = parseScreen(monitor, &error);
    if (screen == nullptr) {
      QTextStream(stderr) << "vosulay: " << error << '\n';
      return 2;
    }
  }

  ShellWindow window(options, screen, icon);
  if (!window.ipcReady()) {
    QTextStream(stderr)
        << "vosulay: another instance is running or IPC could not be started\n";
    return 1;
  }
  window.initializeWindow();
  return application.exec();
}

} // namespace vosulay
