#include "intents.h"

#include <QDBusConnection>
#include <QDBusMessage>
#include <QDir>
#include <QFile>
#include <QJsonArray>
#include <QJsonDocument>
#include <QProcess>
#include <QStandardPaths>
#include <QTemporaryDir>
#include <QTest>

// Marathon Camera's Keel Actions: answers camera.capture when the test says.
class FakeCamera : public QObject {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.shipwright.Keel.Actions")

  public:
    QList<QDBusMessage> calls;
    QStringList         actions;
    QStringList         arguments;

    void answer(const QString &json) {
        QDBusConnection::sessionBus().send(calls.takeFirst().createReply(json));
    }
    void refuse(const QString &code) {
        QDBusConnection::sessionBus().send(calls.takeFirst().createErrorReply(
            QStringLiteral("org.shipwright.Keel.Actions.Error.") + code, QStringLiteral("no")));
    }

  public Q_SLOTS:
    QString Invoke(const QString &action, const QString &args, const QVariantMap &, const QDBusMessage &message) {
        actions << action;
        arguments << args;
        message.setDelayedReply(true);
        calls << message;
        return {};
    }
};

using Intents::Target;

class TestIntents : public QObject {
    Q_OBJECT

    static std::optional<Target> open(const QJsonObject &m) {
        QString error;
        return Intents::target(m, &error);
    }

    QProcess m_bus;

  private Q_SLOTS:
    void initTestCase() {
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

    void links() {
        auto t = open({{QStringLiteral("kind"), QStringLiteral("url")},
                       {QStringLiteral("uri"), QStringLiteral("https://f-droid.org/packages/")}});
        QVERIFY(t);
        QCOMPARE(t->kind, Target::Keel);
        QCOMPARE(t->app, QStringLiteral("browser"));
        QCOMPARE(t->name, QStringLiteral("browser.open"));
        QCOMPARE(t->args.value(QStringLiteral("url")).toString(), QStringLiteral("https://f-droid.org/packages/"));
        QVERIFY(!open({{QStringLiteral("kind"), QStringLiteral("url")},
                       {QStringLiteral("uri"), QStringLiteral("javascript:alert(1)")}}));
        QVERIFY(!open({{QStringLiteral("kind"), QStringLiteral("url")},
                       {QStringLiteral("uri"), QStringLiteral("file:///etc/passwd")}}));
    }

    void numbers() {
        auto t = open({{QStringLiteral("kind"), QStringLiteral("tel")},
                       {QStringLiteral("uri"), QStringLiteral("tel:+353%2087%20123-4567")}});
        QVERIFY(t);
        QCOMPARE(t->kind, Target::Route);
        QCOMPARE(t->app, QStringLiteral("phone"));
        QCOMPARE(t->name, QStringLiteral("/call"));
        QCOMPARE(t->args.value(QStringLiteral("number")).toString(), QStringLiteral("+353871234567"));

        t = open({{QStringLiteral("kind"), QStringLiteral("tel")}, {QStringLiteral("uri"), QStringLiteral("tel:")}});
        QVERIFY(t);
        QCOMPARE(t->kind, Target::Launch);

        t = open({{QStringLiteral("kind"), QStringLiteral("sms")},
                  {QStringLiteral("uri"), QStringLiteral("smsto:0871234567?body=hi")}});
        QVERIFY(t);
        QCOMPARE(t->app, QStringLiteral("messages"));
        QCOMPARE(t->name, QStringLiteral("conversation"));
        QCOMPARE(t->args.value(QStringLiteral("number")).toString(), QStringLiteral("0871234567"));

        QCOMPARE(t->args.value(QStringLiteral("body")).toString(), QStringLiteral("hi"));

        // The body comes along: as the thread's draft, or a new message without a number.
        t = open({{QStringLiteral("kind"), QStringLiteral("sms")},
                  {QStringLiteral("uri"), QStringLiteral("smsto:0871234567")},
                  {QStringLiteral("body"), QStringLiteral("See you at 8")}});
        QVERIFY(t);
        QCOMPARE(t->name, QStringLiteral("conversation"));
        QCOMPARE(t->args.value(QStringLiteral("body")).toString(), QStringLiteral("See you at 8"));
        t = open({{QStringLiteral("kind"), QStringLiteral("sms")},
                  {QStringLiteral("uri"), QStringLiteral("sms:")},
                  {QStringLiteral("body"), QString(20000, QLatin1Char('x'))}});
        QVERIFY(t);
        QCOMPARE(t->kind, Target::Route);
        QCOMPARE(t->app, QStringLiteral("messages"));
        QCOMPARE(t->name, QStringLiteral("compose"));
        QVERIFY(!t->args.contains(QStringLiteral("number")));
        QCOMPARE(t->args.value(QStringLiteral("body")).toString().size(), 10000);
        t = open({{QStringLiteral("kind"), QStringLiteral("sms")}, {QStringLiteral("uri"), QStringLiteral("sms:")}});
        QVERIFY(t);
        QCOMPARE(t->kind, Target::Launch);

        t = open({{QStringLiteral("kind"), QStringLiteral("tel")},
                  {QStringLiteral("uri"), QStringLiteral("tel:$(reboot)")}});
        QVERIFY(t);
        QCOMPARE(t->kind, Target::Launch);
        QVERIFY(!open({{QStringLiteral("kind"), QStringLiteral("tel")},
                       {QStringLiteral("uri"), QStringLiteral("sms:1")}}));
    }

    void mail() {
        auto t = open({{QStringLiteral("kind"), QStringLiteral("mailto")},
                       {QStringLiteral("uri"), QStringLiteral("mailto:a@b.ie")},
                       {QStringLiteral("to"), QJsonArray{QStringLiteral("a@b.ie"), QStringLiteral("bad address")}},
                       {QStringLiteral("subject"), QStringLiteral("Hi")}});
        QVERIFY(t);
        QCOMPARE(t->app, QStringLiteral("email"));
        QCOMPARE(t->name, QStringLiteral("mail.compose"));
        QCOMPARE(t->args.value(QStringLiteral("to")).toString(), QStringLiteral("a@b.ie"));
        QCOMPARE(t->args.value(QStringLiteral("subject")).toString(), QStringLiteral("Hi"));
    }

    void places() {
        auto t = open({{QStringLiteral("kind"), QStringLiteral("geo")},
                       {QStringLiteral("uri"), QStringLiteral("geo:53.3498,-6.2603")}});
        QVERIFY(t);
        QCOMPARE(t->name, QStringLiteral("map.show"));
        QCOMPARE(t->args.value(QStringLiteral("lat")).toDouble(), 53.3498);

        t = open({{QStringLiteral("kind"), QStringLiteral("geo")},
                  {QStringLiteral("uri"), QStringLiteral("geo:0,0?q=53.34,-6.26(Trinity%20College)")}});
        QVERIFY(t);
        QCOMPARE(t->name, QStringLiteral("map.show"));
        QCOMPARE(t->args.value(QStringLiteral("name")).toString(), QStringLiteral("Trinity College"));

        t = open({{QStringLiteral("kind"), QStringLiteral("geo")},
                  {QStringLiteral("uri"), QStringLiteral("geo:0,0?q=coffee%20near%20me")}});
        QVERIFY(t);
        QCOMPARE(t->kind, Target::Keel);
        QCOMPARE(t->app, QStringLiteral("maps"));
        QCOMPARE(t->name, QStringLiteral("search.show"));
        QCOMPARE(t->args.value(QStringLiteral("query")).toString(), QStringLiteral("coffee near me"));
        t = open({{QStringLiteral("kind"), QStringLiteral("geo")},
                  {QStringLiteral("uri"), QStringLiteral("geo:0,0?q=") + QString(300, QLatin1Char('a'))}});
        QCOMPARE(t->args.value(QStringLiteral("query")).toString().size(), 256);

        t = open({{QStringLiteral("kind"), QStringLiteral("geo")}, {QStringLiteral("uri"), QStringLiteral("geo:0,0")}});
        QVERIFY(t);
        QCOMPARE(t->kind, Target::Launch);
        QCOMPARE(t->app, QStringLiteral("maps"));
    }

    void settings() {
        QCOMPARE(Intents::settingsRoute(QStringLiteral("android.settings.WIFI_SETTINGS"), {}), QStringLiteral("wifi"));
        QCOMPARE(Intents::settingsRoute(QStringLiteral("android.settings.APPLICATION_DETAILS_SETTINGS"),
                                        QStringLiteral("org.fdroid.fdroid")),
                 QStringLiteral("appmanager/waydroid.org.fdroid.fdroid"));
        QCOMPARE(Intents::settingsRoute(QStringLiteral("android.settings.APP_NOTIFICATION_SETTINGS"),
                                        QStringLiteral("../x")),
                 QStringLiteral("notifications"));
        QVERIFY(Intents::settingsRoute(QStringLiteral("android.settings.LOCATION_SOURCE_SETTINGS"), {}).isEmpty());
        QCOMPARE(Intents::settingsRoute(QStringLiteral("android.settings.MANAGE_APP_ALL_FILES_ACCESS_PERMISSION"),
                                        QStringLiteral("me.zhanghai.android.files")),
                 QStringLiteral("androidaccess/all_files/me.zhanghai.android.files"));
        QCOMPARE(Intents::settingsRoute(QStringLiteral("android.settings.MANAGE_ALL_FILES_ACCESS_PERMISSION"), {}),
                 QStringLiteral("androidaccess/all_files"));
        QCOMPARE(Intents::settingsRoute(QStringLiteral("android.settings.MANAGE_UNKNOWN_APP_SOURCES"),
                                        QStringLiteral("../../x")),
                 QStringLiteral("androidaccess/install_apps"));
        QCOMPARE(Intents::settingsRoute(QStringLiteral("android.settings.REQUEST_IGNORE_BATTERY_OPTIMIZATIONS"),
                                        QStringLiteral("org.fdroid.fdroid")),
                 QStringLiteral("androidaccess/battery/org.fdroid.fdroid"));
        auto t = open({{QStringLiteral("kind"), QStringLiteral("settings")},
                       {QStringLiteral("action"), QStringLiteral("android.settings.SETTINGS")}});
        QVERIFY(t);
        QCOMPARE(t->kind, Target::Launch);
        QCOMPARE(t->app, QStringLiteral("settings"));
    }

    // A capture asks Marathon Camera's camera.capture and answers with the file it returns.
    void capture() {
        if (m_bus.state() != QProcess::Running)
            QSKIP("no dbus-daemon");
        FakeCamera      camera;
        QDBusConnection bus = QDBusConnection::sessionBus();
        QVERIFY(bus.registerObject(QStringLiteral("/org/shipwright/Keel/Actions"), &camera,
                                   QDBusConnection::ExportAllSlots));
        QVERIFY(bus.registerService(QStringLiteral("org.marathonos.camera")));

        QTemporaryDir      dir;
        const QString      pictures = dir.filePath(QStringLiteral("Pictures/Marathon"));
        QList<QJsonObject> sent;
        QDir().mkpath(pictures);
        auto write = [](const QString &path) {
            QFile f(path);
            QVERIFY(f.open(QIODevice::WriteOnly));
            f.write("jpeg");
        };
        write(pictures + QStringLiteral("/IMG_1.jpg"));
        write(pictures + QStringLiteral("/VID_1.mp4"));
        write(dir.filePath(QStringLiteral("secret.jpg")));

        QDBusConnection client = QDBusConnection::connectToBus(QDBusConnection::SessionBus, QStringLiteral("intents"));
        SystemIntents intents([&](const QJsonObject &m) { sent << m; }, dir.filePath(QStringLiteral("xfer")), pictures,
                              client);
        auto pick = [&](const QString &id, const QString &kind) {
            intents.handle(QStringLiteral("pick"), {{QStringLiteral("t"), QStringLiteral("pick")},
                                                    {QStringLiteral("id"), id},
                                                    {QStringLiteral("mode"), QStringLiteral("capture")},
                                                    {QStringLiteral("kind"), kind}});
            QTRY_COMPARE(camera.calls.size(), 1);
        };
        auto last = [&]() { return sent.isEmpty() ? QJsonObject() : sent.last(); };

        pick(QStringLiteral("00000000000000a1"), QStringLiteral("image"));
        QCOMPARE(camera.actions.last(), QStringLiteral("camera.capture"));
        QCOMPARE(QJsonDocument::fromJson(camera.arguments.last().toUtf8()).object().value(QStringLiteral("mode")).toString(),
                 QStringLiteral("photo"));
        QVERIFY(sent.isEmpty());
        camera.answer(QString::fromUtf8(QJsonDocument(QJsonObject{{QStringLiteral("path"), pictures + QStringLiteral("/IMG_1.jpg")},
                                                                 {QStringLiteral("mode"), QStringLiteral("photo")}})
                                            .toJson(QJsonDocument::Compact)));
        QTRY_COMPARE(sent.size(), 1);
        QCOMPARE(last().value(QStringLiteral("t")).toString(), QStringLiteral("picked"));
        QCOMPARE(last().value(QStringLiteral("ok")).toBool(), true);
        QCOMPARE(last().value(QStringLiteral("files")).toArray().first().toObject().value(QStringLiteral("name")).toString(),
                 QStringLiteral("IMG_1.jpg"));
        QVERIFY(QFile::exists(dir.filePath(QStringLiteral("xfer/00000000000000a1/IMG_1.jpg"))));

        // Only files in Marathon Camera's folder, of the kind asked for.
        pick(QStringLiteral("00000000000000a2"), QStringLiteral("image"));
        camera.answer(QStringLiteral("{\"path\":\"%1\",\"mode\":\"photo\"}").arg(dir.filePath(QStringLiteral("secret.jpg"))));
        QTRY_COMPARE(sent.size(), 2);
        QCOMPARE(last().value(QStringLiteral("ok")).toBool(), false);
        QVERIFY(!QFile::exists(dir.filePath(QStringLiteral("xfer/00000000000000a2/secret.jpg"))));
        pick(QStringLiteral("00000000000000a3"), QStringLiteral("image"));
        camera.answer(QStringLiteral("{\"path\":\"%1/VID_1.mp4\",\"mode\":\"video\"}").arg(pictures));
        QTRY_COMPARE(sent.size(), 3);
        QCOMPARE(last().value(QStringLiteral("ok")).toBool(), false);

        // The person left the camera.
        pick(QStringLiteral("00000000000000a4"), QStringLiteral("video"));
        QCOMPARE(QJsonDocument::fromJson(camera.arguments.last().toUtf8()).object().value(QStringLiteral("mode")).toString(),
                 QStringLiteral("video"));
        camera.refuse(QStringLiteral("Cancelled"));
        QTRY_COMPARE(sent.size(), 4);
        QCOMPARE(last().value(QStringLiteral("ok")).toBool(), false);

        // Android gave up first: the late answer goes nowhere.
        pick(QStringLiteral("00000000000000a5"), QStringLiteral("video"));
        intents.handle(QStringLiteral("pick_cancel"), {{QStringLiteral("id"), QStringLiteral("00000000000000a5")}});
        camera.answer(QStringLiteral("{\"path\":\"%1/VID_1.mp4\",\"mode\":\"video\"}").arg(pictures));
        QTest::qWait(200);
        QCOMPARE(sent.size(), 4);
    }

    void namesAndIds() {
        QVERIFY(Intents::isRequestId(QStringLiteral("0123456789abcdef")));
        QVERIFY(!Intents::isRequestId(QStringLiteral("../0123456789abc")));
        QCOMPARE(Intents::safeName(QStringLiteral("/home/user/Documents/report.pdf")), QStringLiteral("report.pdf"));
        QCOMPARE(Intents::safeName(QStringLiteral("..")), QStringLiteral("file"));
        QCOMPARE(Intents::safeName(QStringLiteral("a\nb.txt")), QStringLiteral("ab.txt"));
    }
};

QTEST_GUILESS_MAIN(TestIntents)
#include "test_intents.moc"
