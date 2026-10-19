#include "location.h"

#include <QDBusArgument>
#include <QDBusConnection>
#include <QDBusMetaType>
#include <QJsonObject>
#include <QProcess>
#include <QSignalSpy>
#include <QStandardPaths>
#include <QTest>

#include <limits>

struct Stamp {
    qulonglong seconds = 0;
    qulonglong micros  = 0;
};
Q_DECLARE_METATYPE(Stamp)

bool operator==(const Stamp &a, const Stamp &b) {
    return a.seconds == b.seconds && a.micros == b.micros;
}

QDBusArgument &operator<<(QDBusArgument &a, const Stamp &s) {
    a.beginStructure();
    a << s.seconds << s.micros;
    a.endStructure();
    return a;
}
const QDBusArgument &operator>>(const QDBusArgument &a, Stamp &s) {
    a.beginStructure();
    a >> s.seconds >> s.micros;
    a.endStructure();
    return a;
}

class FakeLocation : public QObject {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.freedesktop.GeoClue2.Location")
    Q_PROPERTY(double Latitude MEMBER latitude)
    Q_PROPERTY(double Longitude MEMBER longitude)
    Q_PROPERTY(double Accuracy MEMBER accuracy)
    Q_PROPERTY(double Altitude MEMBER altitude)
    Q_PROPERTY(double Speed MEMBER speed)
    Q_PROPERTY(double Heading MEMBER heading)
    Q_PROPERTY(Stamp Timestamp MEMBER timestamp)

  public:
    double latitude = 53.35, longitude = -6.26, accuracy = 25000;
    double altitude = -std::numeric_limits<double>::max(), speed = -1, heading = -1;
    Stamp  timestamp{1790000000, 500000};
};

class FakeClient : public QObject {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.freedesktop.GeoClue2.Client")
    Q_PROPERTY(QString DesktopId MEMBER desktopId)
    Q_PROPERTY(uint RequestedAccuracyLevel MEMBER accuracy)
    Q_PROPERTY(uint TimeThreshold MEMBER threshold)

  public:
    QString desktopId;
    uint    accuracy = 0, threshold = 0;
    int     starts = 0, stops = 0;

  public Q_SLOTS:
    void Start() { ++starts; }
    void Stop() { ++stops; }

  Q_SIGNALS:
    void LocationUpdated(const QDBusObjectPath &oldPath, const QDBusObjectPath &newPath);
};

class FakeManager : public QObject {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.freedesktop.GeoClue2.Manager")

  public:
    int clients = 0;

  public Q_SLOTS:
    QDBusObjectPath GetClient() {
        ++clients;
        return QDBusObjectPath(QStringLiteral("/org/freedesktop/GeoClue2/Client/1"));
    }
};

class TestLocation : public QObject {
    Q_OBJECT

    QProcess m_bus;

  private Q_SLOTS:
    void initTestCase() {
        qDBusRegisterMetaType<Stamp>();
        const QString daemon = QStandardPaths::findExecutable(QStringLiteral("dbus-daemon"));
        if (daemon.isEmpty())
            return;
        m_bus.start(daemon, {QStringLiteral("--session"), QStringLiteral("--nofork"), QStringLiteral("--print-address")});
        QVERIFY(m_bus.waitForReadyRead(5000));
        qputenv("DBUS_SESSION_BUS_ADDRESS", m_bus.readLine().trimmed());
    }

    void cleanupTestCase() {
        m_bus.kill();
        m_bus.waitForFinished();
    }

    void message() {
        const QVariantMap place{{QStringLiteral("Latitude"), 53.35},  {QStringLiteral("Longitude"), -6.26},
                                {QStringLiteral("Accuracy"), 40.0},   {QStringLiteral("Altitude"), 12.5},
                                {QStringLiteral("Speed"), -1.0},      {QStringLiteral("Heading"), 90.0},
                                {QStringLiteral("Timestamp"), qint64(1790000000500)}};
        const QJsonObject m = LocationSync::message(place);
        QCOMPARE(m.value(QStringLiteral("t")).toString(), QStringLiteral("location"));
        QCOMPARE(m.value(QStringLiteral("lat")).toDouble(), 53.35);
        QCOMPARE(m.value(QStringLiteral("acc")).toDouble(), 40.0);
        QCOMPARE(m.value(QStringLiteral("alt")).toDouble(), 12.5);
        QCOMPARE(m.value(QStringLiteral("heading")).toDouble(), 90.0);
        QVERIFY(!m.contains(QStringLiteral("speed")));
        QCOMPARE(m.value(QStringLiteral("time")).toDouble(), 1790000000500.0);

        QVariantMap bad = place;
        bad.insert(QStringLiteral("Latitude"), 91.0);
        QVERIFY(LocationSync::message(bad).isEmpty());
        bad = place;
        bad.insert(QStringLiteral("Accuracy"), 0.0);
        QVERIFY(LocationSync::message(bad).isEmpty());
        bad = place;
        bad.insert(QStringLiteral("Longitude"), std::numeric_limits<double>::quiet_NaN());
        QVERIFY(LocationSync::message(bad).isEmpty());
        bad = place;
        bad.remove(QStringLiteral("Timestamp"));
        QVERIFY(LocationSync::message(bad).isEmpty());
    }

    void followsAndroidsRequest() {
        if (m_bus.state() != QProcess::Running)
            QSKIP("no dbus-daemon");
        QDBusConnection geoclue = QDBusConnection::connectToBus(QDBusConnection::SessionBus, QStringLiteral("geoclue"));
        FakeManager     manager;
        FakeClient      client;
        FakeLocation    place;
        const auto      all = QDBusConnection::ExportAllContents;
        QVERIFY(geoclue.registerObject(QStringLiteral("/org/freedesktop/GeoClue2/Manager"), &manager, all));
        QVERIFY(geoclue.registerObject(QStringLiteral("/org/freedesktop/GeoClue2/Client/1"), &client, all));
        QVERIFY(geoclue.registerObject(QStringLiteral("/org/freedesktop/GeoClue2/Location/1"), &place, all));
        QVERIFY(geoclue.registerService(QStringLiteral("org.freedesktop.GeoClue2")));

        QList<QJsonObject> sent;
        LocationSync       sync(QDBusConnection::sessionBus(), [&](const QJsonObject &m) { sent.append(m); });

        sync.handle(QStringLiteral("location_request"),
                    {{QStringLiteral("on"), true}, {QStringLiteral("interval"), 60000}});
        QTRY_COMPARE(client.starts, 1);
        QCOMPARE(manager.clients, 1);
        QCOMPARE(client.desktopId, QStringLiteral("org.marathonos.Deckard"));
        QCOMPARE(client.accuracy, 8u);
        QTRY_COMPARE(client.threshold, 60u);

        Q_EMIT client.LocationUpdated(QDBusObjectPath(QStringLiteral("/")),
                                      QDBusObjectPath(QStringLiteral("/org/freedesktop/GeoClue2/Location/1")));
        QTRY_COMPARE(sent.size(), 1);
        QCOMPARE(sent.at(0).value(QStringLiteral("lon")).toDouble(), -6.26);
        QCOMPARE(sent.at(0).value(QStringLiteral("acc")).toDouble(), 25000.0);
        QCOMPARE(sent.at(0).value(QStringLiteral("time")).toDouble(), 1790000000500.0);
        QVERIFY(!sent.at(0).contains(QStringLiteral("alt")));

        // A new interval while running changes the threshold, not the client.
        sync.handle(QStringLiteral("location_request"),
                    {{QStringLiteral("on"), true}, {QStringLiteral("interval"), 5000}});
        QTRY_COMPARE(client.threshold, 5u);
        QCOMPARE(client.starts, 1);

        sync.handle(QStringLiteral("location_request"), {{QStringLiteral("on"), false}});
        QTRY_COMPARE(client.stops, 1);
        Q_EMIT client.LocationUpdated(QDBusObjectPath(QStringLiteral("/")),
                                      QDBusObjectPath(QStringLiteral("/org/freedesktop/GeoClue2/Location/1")));
        QTest::qWait(200);
        QCOMPARE(sent.size(), 1);

        sync.handle(QStringLiteral("location_request"), {{QStringLiteral("on"), true}});
        QTRY_COMPARE(client.starts, 2);
        QCOMPARE(manager.clients, 1);
        sync.bridgeLost();
        QTRY_COMPARE(client.stops, 2);
    }
};

QTEST_GUILESS_MAIN(TestLocation)
#include "test_location.moc"
