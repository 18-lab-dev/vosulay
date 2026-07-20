#pragma once

#include <QObject>
#include <QString>

#include <functional>

namespace vosulay {

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
