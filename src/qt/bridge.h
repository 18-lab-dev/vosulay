#pragma once

#include <QObject>
#include <QString>
#include <QVariantMap>

#include <functional>

namespace vosulay {

class PortalResponseReceiver final : public QObject {
  Q_OBJECT

public:
  explicit PortalResponseReceiver(
      std::function<void(uint, const QVariantMap &)> callback,
      QObject *parent = nullptr);

public slots:
  void response(uint response, const QVariantMap &results);

private:
  std::function<void(uint, const QVariantMap &)> callback_;
};

class ManagerBridge final : public QObject {
  Q_OBJECT

public:
  explicit ManagerBridge(std::function<void(const QString &)> handler,
                         QObject *parent = nullptr);

public slots:
  void postMessage(const QString &message);

private:
  std::function<void(const QString &)> handler_;
};

} // namespace vosulay
