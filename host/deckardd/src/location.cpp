#include "location.h"

#include <QDBusArgument>
#include <QDBusMessage>
#include <QDBusPendingCallWatcher>
#include <QDBusPendingReply>
#include <QDBusServiceWatcher>
#include <QDBusVariant>
#include <QLoggingCategory>

#include <cmath>

namespace {
    const QString kGeoClue  = QStringLiteral("org.freedesktop.GeoClue2");
    const QString kManager  = QStringLiteral("/org/freedesktop/GeoClue2/Manager");
    const QString kClient   = QStringLiteral("org.freedesktop.GeoClue2.Client");
    const QString kLocation = QStringLiteral("org.freedesktop.GeoClue2.Location");
    const QString kProps    = QStringLiteral("org.freedesktop.DBus.Properties");
    // GeoClue's "exact": Android coarsens it for apps with only approximate access.
    constexpr uint kAccuracyExact = 8;

    bool finite(const QVariant &v, double *out) {
        bool         ok = false;
        const double d  = v.toDouble(&ok);
        if (!ok || !std::isfinite(d))
            return false;
        *out = d;
        return true;
    }
} // namespace

const QString &LocationSync::desktopId() {
    static const QString id = QStringLiteral("org.marathonos.Deckard");
    return id;
}

bool LocationSync::handles(const QString &type) {
    return type == QLatin1String("location_request");
}

LocationSync::LocationSync(const QDBusConnection &bus, Send send, QObject *parent)
    : QObject(parent)
    , m_bus(bus)
    , m_send(std::move(send)) {
    auto *watcher = new QDBusServiceWatcher(kGeoClue, m_bus, QDBusServiceWatcher::WatchForOwnerChange, this);
    connect(watcher, &QDBusServiceWatcher::serviceOwnerChanged, this, &LocationSync::onOwnerChanged);
}

void LocationSync::handle(const QString &type, const QJsonObject &message) {
    if (!handles(type))
        return;
    const bool   on       = message.value(QStringLiteral("on")).toBool(false);
    const double interval = message.value(QStringLiteral("interval")).toDouble(0);
    m_interval            = std::isfinite(interval) ? qBound<qint64>(0, qint64(interval), kMaxIntervalMs) : 0;
    m_wanted              = on;
    apply();
}

void LocationSync::bridgeLost() {
    m_wanted = false;
    apply();
}

void LocationSync::apply() {
    if (m_wanted && m_client.isEmpty()) {
        createClient();
        return;
    }
    if (m_client.isEmpty())
        return;
    if (m_wanted) {
        setProperty(QStringLiteral("TimeThreshold"), uint(m_interval / 1000));
        if (m_started)
            return;
        m_started = true;
        auto *call = new QDBusPendingCallWatcher(
            m_bus.asyncCall(QDBusMessage::createMethodCall(kGeoClue, m_client, kClient, QStringLiteral("Start"))),
            this);
        connect(call, &QDBusPendingCallWatcher::finished, this, [this, call] {
            call->deleteLater();
            if (call->isError()) {
                qWarning("GeoClue refused Android's location: %s", qPrintable(call->error().message()));
                m_started = false;
            }
        });
    } else if (m_started) {
        m_started = false;
        m_bus.asyncCall(QDBusMessage::createMethodCall(kGeoClue, m_client, kClient, QStringLiteral("Stop")));
    }
}

void LocationSync::createClient() {
    if (m_creating)
        return;
    m_creating = true;
    auto *call = new QDBusPendingCallWatcher(
        m_bus.asyncCall(QDBusMessage::createMethodCall(kGeoClue, kManager, QStringLiteral("org.freedesktop.GeoClue2.Manager"),
                                                       QStringLiteral("GetClient"))),
        this);
    connect(call, &QDBusPendingCallWatcher::finished, this, [this, call] {
        call->deleteLater();
        m_creating = false;
        QDBusPendingReply<QDBusObjectPath> reply = *call;
        if (reply.isError()) {
            qWarning("No GeoClue client for Android: %s", qPrintable(reply.error().message()));
            return;
        }
        m_client  = reply.value().path();
        m_started = false;
        setProperty(QStringLiteral("DesktopId"), desktopId());
        setProperty(QStringLiteral("RequestedAccuracyLevel"), kAccuracyExact);
        m_bus.connect(kGeoClue, m_client, kClient, QStringLiteral("LocationUpdated"), this,
                      SLOT(onLocationUpdated(QDBusObjectPath, QDBusObjectPath)));
        apply();
    });
}

void LocationSync::setProperty(const QString &name, const QVariant &value) {
    QDBusMessage m = QDBusMessage::createMethodCall(kGeoClue, m_client, kProps, QStringLiteral("Set"));
    m.setArguments({kClient, name, QVariant::fromValue(QDBusVariant(value))});
    m_bus.asyncCall(m);
}

void LocationSync::onLocationUpdated(const QDBusObjectPath &, const QDBusObjectPath &newLocation) {
    QDBusMessage m = QDBusMessage::createMethodCall(kGeoClue, newLocation.path(), kProps, QStringLiteral("GetAll"));
    m.setArguments({kLocation});
    auto *call = new QDBusPendingCallWatcher(m_bus.asyncCall(m), this);
    connect(call, &QDBusPendingCallWatcher::finished, this, [this, call] {
        call->deleteLater();
        QDBusPendingReply<QVariantMap> reply = *call;
        if (reply.isError() || !m_wanted)
            return;
        QVariantMap   props = reply.value();
        const QVariant ts   = props.value(QStringLiteral("Timestamp"));
        if (ts.userType() == qMetaTypeId<QDBusArgument>()) {
            const QDBusArgument arg = ts.value<QDBusArgument>();
            qulonglong          seconds = 0, micros = 0;
            arg.beginStructure();
            arg >> seconds >> micros;
            arg.endStructure();
            props.insert(QStringLiteral("Timestamp"), qint64(seconds * 1000 + micros / 1000));
        }
        const QJsonObject out = message(props);
        if (!out.isEmpty())
            m_send(out);
    });
}

QJsonObject LocationSync::message(const QVariantMap &location) {
    double lat = 0, lon = 0, accuracy = 0, time = 0;
    if (!finite(location.value(QStringLiteral("Latitude")), &lat) || lat < -90 || lat > 90 ||
        !finite(location.value(QStringLiteral("Longitude")), &lon) || lon < -180 || lon > 180 ||
        !finite(location.value(QStringLiteral("Accuracy")), &accuracy) || accuracy <= 0 ||
        !finite(location.value(QStringLiteral("Timestamp")), &time) || time <= 0)
        return {};
    QJsonObject out{{QStringLiteral("t"), QStringLiteral("location")},
                    {QStringLiteral("lat"), lat},
                    {QStringLiteral("lon"), lon},
                    {QStringLiteral("acc"), std::min(accuracy, 1e7)},
                    {QStringLiteral("time"), time}};
    double v = 0;
    // GeoClue's "unknown" values: -DBL_MAX altitude, -1 speed and heading.
    if (finite(location.value(QStringLiteral("Altitude")), &v) && v > -1e5 && v < 1e5)
        out.insert(QStringLiteral("alt"), v);
    if (finite(location.value(QStringLiteral("Speed")), &v) && v >= 0 && v < 1e4)
        out.insert(QStringLiteral("speed"), v);
    if (finite(location.value(QStringLiteral("Heading")), &v) && v >= 0 && v < 360)
        out.insert(QStringLiteral("heading"), v);
    return out;
}

void LocationSync::onOwnerChanged(const QString &, const QString &oldOwner, const QString &) {
    // GeoClue exits when idle and forgets its clients; starting it is no loss.
    if (oldOwner.isEmpty() || m_client.isEmpty())
        return;
    m_bus.disconnect(kGeoClue, m_client, kClient, QStringLiteral("LocationUpdated"), this,
                     SLOT(onLocationUpdated(QDBusObjectPath, QDBusObjectPath)));
    m_client.clear();
    m_creating = false;
    m_started  = false;
    if (m_wanted)
        apply();
}
