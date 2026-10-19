#pragma once

#include <QHash>
#include <QJsonObject>
#include <QObject>
#include <QStringList>
#include <QVariantMap>

#include <functional>

class QTimer;

// Bounds and cleans what DeckardSystem answers to a "manage" request
// (protocol/bridge.md, management), as Validate does for the bridge.
namespace ManageCheck {

    const QStringList &groups();
    const QStringList &specialKinds();
    bool               isGroup(const QString &group);
    bool               isSpecialKind(const QString &kind);
    bool               isBatteryMode(const QString &mode);

    QVariantMap  app(const QJsonObject &reply);
    QVariantList special(const QJsonObject &reply);
    QVariantMap  info(const QJsonObject &reply);

} // namespace ManageCheck

// Requests to DeckardSystem over the system channel, answered by id.
class Manage : public QObject {
    Q_OBJECT

  public:
    using Send = std::function<void(const QJsonObject &)>;
    using Done = std::function<void(bool ok, const QJsonObject &reply)>;

    static constexpr int kTimeoutMs  = 30000;
    static constexpr int kMaxPending = 32;

    explicit Manage(Send send, QObject *parent = nullptr);

    bool isConnected() const;
    void request(const QString &op, QJsonObject args, Done done);

  public Q_SLOTS:
    void handle(const QString &type, const QJsonObject &message);
    void setConnected(bool connected);

  Q_SIGNALS:
    void connectedChanged(bool connected);

  private:
    struct Pending {
        Done   done;
        qint64 deadline = 0;
    };
    void expire();
    void finish(const QString &id, bool ok, const QJsonObject &reply);

    Send                    m_send;
    bool                    m_connected = false;
    quint64                 m_next      = 0;
    QHash<QString, Pending> m_pending;
    QTimer                 *m_timer = nullptr;
};
