#pragma once

#include <QDBusConnection>
#include <QDBusObjectPath>
#include <QJsonObject>
#include <QObject>

#include <functional>

// Marathon's location as Android's network location provider (DeckardBridge's
// NetworkLocation, protocol/bridge.md): one GeoClue client while Android has
// a request, none otherwise. Android decides which apps get it and how
// precisely; GeoClue asks the shell's agent whether Android may locate.
class LocationSync : public QObject {
    Q_OBJECT

  public:
    using Send = std::function<void(const QJsonObject &)>;

    static constexpr qint64 kMaxIntervalMs = 24LL * 3600 * 1000;

    LocationSync(const QDBusConnection &bus, Send send, QObject *parent = nullptr);

    static const QString &desktopId();
    static bool           handles(const QString &type);
    void                  handle(const QString &type, const QJsonObject &message);
    void                  bridgeLost();

    // A GeoClue Location's properties as the bridge's "location" message;
    // empty when they don't describe a place.
    static QJsonObject message(const QVariantMap &location);

  private Q_SLOTS:
    void onLocationUpdated(const QDBusObjectPath &oldLocation, const QDBusObjectPath &newLocation);
    void onOwnerChanged(const QString &name, const QString &oldOwner, const QString &newOwner);

  private:
    void apply();
    void createClient();
    void setProperty(const QString &name, const QVariant &value);

    QDBusConnection m_bus;
    Send            m_send;
    bool            m_wanted   = false;
    qint64          m_interval = 0;
    QString         m_client;
    bool            m_creating = false;
    bool            m_started  = false;
};
