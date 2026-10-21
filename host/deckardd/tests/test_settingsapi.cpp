#include "callers.h"
#include "manage.h"
#include "permissionmap.h"
#include "runtime.h"
#include "settingsapi.h"

#include <QCoreApplication>
#include <QDBusArgument>
#include <QDBusConnection>
#include <QDBusMessage>
#include <QDBusPendingCall>
#include <QDBusPendingCallWatcher>
#include <QFile>
#include <QImage>
#include <QJsonArray>
#include <QProcess>
#include <QSignalSpy>
#include <QStandardPaths>
#include <QTemporaryDir>
#include <QTest>
#include <QTimer>

// The shell's Navigation1, answering AppIdForPid for this test process.
class FakeShell : public QObject {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.marathonos.Shell.Navigation1")

  public:
    QString appId;

  public Q_SLOTS:
    QString AppIdForPid(uint pid) {
        return qint64(pid) == QCoreApplication::applicationPid() ? appId : QString();
    }
};

class TestSettingsApi : public QObject {
    Q_OBJECT

    QProcess m_bus;

    static void writeFile(const QString &path, const QByteArray &bytes) {
        QFile f(path);
        QVERIFY(f.open(QIODevice::WriteOnly));
        f.write(bytes);
    }

    // A call from a connection of its own, as Settings' would be.
    static QDBusMessage call(const QString &method, const QVariantList &args = {}) {
        QDBusConnection client = QDBusConnection::connectToBus(QDBusConnection::SessionBus,
                                                               QStringLiteral("client"));
        QDBusMessage m = QDBusMessage::createMethodCall(QStringLiteral("org.marathonos.DeckardTest"),
                                                        QStringLiteral("/org/marathonos/Deckard"),
                                                        QStringLiteral("org.marathonos.Deckard1"), method);
        m.setArguments(args);
        QDBusPendingCallWatcher watcher(client.asyncCall(m, 5000));
        if (!QTest::qWaitFor([&] { return watcher.isFinished(); }, 6000))
            return {};
        return watcher.reply();
    }

    static QVariantMap map(const QVariant &v) {
        return qdbus_cast<QVariantMap>(v);
    }

  private Q_SLOTS:
    void initTestCase() {
        const QString daemon = QStandardPaths::findExecutable(QStringLiteral("dbus-daemon"));
        if (daemon.isEmpty())
            return;
        m_bus.start(daemon, {QStringLiteral("--session"), QStringLiteral("--nofork"),
                             QStringLiteral("--print-address")});
        QVERIFY(m_bus.waitForReadyRead(5000));
        qputenv("DBUS_SESSION_BUS_ADDRESS", m_bus.readLine().trimmed());
    }

    void cleanupTestCase() {
        m_bus.kill();
        m_bus.waitForFinished();
    }

    void runtimeStates() {
        QTemporaryDir dir;
        QStringList   calls;
        Runtime runtime({dir.filePath(QStringLiteral("memory.current")), dir.filePath(QStringLiteral("state/settings.json")),
                         dir.path()},
                        [&](const QString &m) { calls.append(m); });
        writeFile(dir.filePath(QStringLiteral("memory.current")), "734003200\n");
        QCOMPARE(runtime.state(), QStringLiteral("stopped"));
        QCOMPARE(runtime.snapshot().value(QStringLiteral("memoryBytes")).toLongLong(), 0);
        runtime.setUnit(QStringLiteral("activating"), 0);
        QCOMPARE(runtime.state(), QStringLiteral("starting"));
        runtime.setUnit(QStringLiteral("active"), QDateTime::currentMSecsSinceEpoch() - 90000);
        QCOMPARE(runtime.state(), QStringLiteral("starting"));
        runtime.setAndroidConnected(true);
        runtime.setAndroidVersion(QStringLiteral("13"));
        const QVariantMap s = runtime.snapshot();
        QCOMPARE(s.value(QStringLiteral("state")).toString(), QStringLiteral("running"));
        QCOMPARE(s.value(QStringLiteral("memoryBytes")).toLongLong(), 734003200);
        QCOMPARE(s.value(QStringLiteral("androidVersion")).toString(), QStringLiteral("13"));
        QVERIFY(qAbs(s.value(QStringLiteral("uptimeSeconds")).toLongLong() - 90) <= 2);
        runtime.setUnit(QStringLiteral("deactivating"), 0);
        QCOMPARE(runtime.state(), QStringLiteral("stopping"));
        runtime.setUnit(QStringLiteral("failed"), 0);
        QCOMPARE(runtime.state(), QStringLiteral("stopped"));
    }

    // On by default once an Android app is installed; then what Settings chose.
    void startAtLogin() {
        QTemporaryDir dir;
        QStringList   calls;
        const QString policy = dir.filePath(QStringLiteral("state/settings.json"));
        Runtime runtime({QString(), policy, dir.path()}, [&](const QString &m) { calls.append(m); });
        writeFile(dir.filePath(QStringLiteral("waydroid.com.android.settings.desktop")),
                  "[Desktop Entry]\nName=Settings\nNoDisplay=true\n");
        QVERIFY(!runtime.hasAndroidApps());
        QVERIFY(!runtime.startAtLogin());
        runtime.atLogin();
        QVERIFY(calls.isEmpty());

        writeFile(dir.filePath(QStringLiteral("waydroid.org.fdroid.fdroid.desktop")),
                  "[Desktop Entry]\nName=F-Droid\nExec=waydroid app launch org.fdroid.fdroid\n");
        QVERIFY(runtime.startAtLogin());
        QVERIFY(!runtime.startAtLoginIsSet());
        runtime.atLogin();
        QCOMPARE(calls, QStringList{QStringLiteral("StartUnit")});

        QSignalSpy changed(&runtime, &Runtime::changed);
        runtime.setStartAtLogin(false);
        QCOMPARE(changed.size(), 1);
        QVERIFY(runtime.startAtLoginIsSet());
        QVERIFY(!runtime.startAtLogin());
        Runtime again({QString(), policy, dir.path()}, [&](const QString &m) { calls.append(m); });
        QVERIFY(!again.startAtLogin());
        again.atLogin();
        QCOMPARE(calls.size(), 1);
    }

    // Android stops first; only then does the helper run.
    void resets() {
        QTemporaryDir dir;
        const QString marker = dir.filePath(QStringLiteral("ran"));
        QStringList   calls;
        Runtime      *runtimePtr = nullptr;
        Runtime runtime({QString(), dir.filePath(QStringLiteral("settings.json")), dir.path(),
                         {QStringLiteral("sh"), QStringLiteral("-c"),
                          QStringLiteral("touch '%1'; exit ${RESET_EXIT:-0}").arg(marker)}},
                        [&](const QString &m) {
                            calls.append(m);
                            if (m == QLatin1String("StopUnit")) {
                                runtimePtr->setUnit(QStringLiteral("deactivating"), 0);
                                QTimer::singleShot(200, runtimePtr, [runtimePtr] {
                                    runtimePtr->setUnit(QStringLiteral("inactive"), 0);
                                });
                            }
                        });
        runtimePtr = &runtime;
        runtime.setUnit(QStringLiteral("active"), 1);
        runtime.setAndroidConnected(true);

        bool    finished = false, ok = false;
        QString error;
        runtime.reset([&](bool o, const QString &e) { finished = true; ok = o; error = e; });
        QCOMPARE(calls, QStringList{QStringLiteral("StopUnit")});
        QTest::qWait(100);
        QVERIFY(!QFile::exists(marker));
        bool again = true;
        runtime.reset([&](bool o, const QString &) { again = o; });
        QVERIFY(!again);
        QTRY_VERIFY(finished);
        QVERIFY2(ok, qPrintable(error));
        QVERIFY(QFile::exists(marker));
        QCOMPARE(calls.size(), 1);

        // Stopped already: straight to the helper, and its failure comes back.
        qputenv("RESET_EXIT", "1");
        finished = false;
        runtime.reset([&](bool o, const QString &e) { finished = true; ok = o; error = e; });
        QTRY_VERIFY(finished);
        qunsetenv("RESET_EXIT");
        QVERIFY(!ok);
        QCOMPARE(error, QStringLiteral("Android's data couldn't be removed"));

        Runtime missing({QString(), QString(), dir.path(), {dir.filePath(QStringLiteral("no-such-helper"))}},
                        [](const QString &) {});
        finished = false;
        missing.reset([&](bool o, const QString &e) { finished = true; ok = o; error = e; });
        QTRY_VERIFY(finished);
        QVERIFY(!ok);
        QCOMPARE(error, QStringLiteral("Resetting Android isn't available"));
    }

    // One table, both ways.
    void permissionMap() {
        QVERIFY(PermissionMap::isName(QStringLiteral("camera")));
        QVERIFY(!PermissionMap::isName(QStringLiteral("system")));
        QCOMPARE(PermissionMap::marathon(QStringLiteral("android.permission.READ_MEDIA_VIDEO")), QStringLiteral("storage"));
        QCOMPARE(PermissionMap::marathon(QStringLiteral("android.permission.NEARBY_WIFI_DEVICES")), QStringLiteral("bluetooth"));
        QVERIFY(PermissionMap::marathon(QStringLiteral("android.permission.INSTALL_PACKAGES")).isEmpty());
        for (const QString &name : PermissionMap::names())
            for (const QString &p : PermissionMap::android(name))
                QCOMPARE(PermissionMap::marathon(p), name);
        QCOMPARE(PermissionMap::toJson().size(), PermissionMap::names().size());
    }

    void cleansReplies() {
        // Android's grants, named as Marathon names them.
        auto perm = [](const char *name, bool granted, bool fixed) {
            return QJsonObject{{QStringLiteral("name"), QStringLiteral("android.permission.") + QLatin1String(name)},
                               {QStringLiteral("granted"), granted},
                               {QStringLiteral("fixed"), fixed}};
        };
        QJsonArray perms{perm("READ_MEDIA_IMAGES", false, false), perm("CAMERA", true, false),
                         perm("READ_MEDIA_AUDIO", true, false), perm("ACCESS_FINE_LOCATION", false, true),
                         perm("ACCESS_COARSE_LOCATION", false, true), perm("POST_NOTIFICATIONS", true, false),
                         perm("INSTALL_PACKAGES", true, false), QJsonObject{{QStringLiteral("name"), 5}}};
        QVariantMap app = ManageCheck::app({{QStringLiteral("pkg"), QStringLiteral("org.fdroid.fdroid")},
                                            {QStringLiteral("label"), QString(300, QLatin1Char('x'))},
                                            {QStringLiteral("dataBytes"), -5},
                                            {QStringLiteral("appBytes"), 1e300},
                                            {QStringLiteral("battery"), QStringLiteral("rm -rf")},
                                            {QStringLiteral("domains"), QJsonArray{QStringLiteral("f-droid.org"), QStringLiteral("a b"), QStringLiteral("*.f-droid.org")}},
                                            {QStringLiteral("perms"), perms}});
        QCOMPARE(app.value(QStringLiteral("label")).toString().size(), 128);
        QCOMPARE(app.value(QStringLiteral("dataBytes")).toLongLong(), 0);
        QCOMPARE(app.value(QStringLiteral("appBytes")).toLongLong(), qint64(1) << 50);
        QCOMPARE(app.value(QStringLiteral("battery")).toString(), QStringLiteral("optimized"));
        QCOMPARE(app.value(QStringLiteral("domains")).toStringList(),
                 (QStringList{QStringLiteral("f-droid.org"), QStringLiteral("*.f-droid.org")}));
        const QVariantList p = app.value(QStringLiteral("permissions")).toList();
        QCOMPARE(p.size(), 3);
        QCOMPARE(p[0].toMap().value(QStringLiteral("name")).toString(), QStringLiteral("location"));
        QVERIFY(!p[0].toMap().value(QStringLiteral("granted")).toBool());
        QVERIFY(p[0].toMap().value(QStringLiteral("fixed")).toBool());
        QCOMPARE(p[1].toMap().value(QStringLiteral("name")).toString(), QStringLiteral("camera"));
        QVERIFY(p[1].toMap().value(QStringLiteral("granted")).toBool());
        QCOMPARE(p[2].toMap().value(QStringLiteral("name")).toString(), QStringLiteral("storage"));
        QVERIFY(p[2].toMap().value(QStringLiteral("granted")).toBool());
        QVERIFY(!p[2].toMap().value(QStringLiteral("fixed")).toBool());
        QVERIFY(ManageCheck::app({{QStringLiteral("pkg"), QStringLiteral("../../etc")}}).isEmpty());

        const QVariantList special = ManageCheck::special(
            {{QStringLiteral("apps"), QJsonArray{QJsonObject{{QStringLiteral("pkg"), QStringLiteral("a.b")}, {QStringLiteral("granted"), true}},
                                                 QJsonObject{{QStringLiteral("pkg"), QStringLiteral("a.b")}},
                                                 QJsonObject{{QStringLiteral("pkg"), QStringLiteral("$x")}}}}});
        QCOMPARE(special.size(), 1);
        QCOMPARE(special[0].toMap().value(QStringLiteral("label")).toString(), QStringLiteral("a.b"));

        // Push only counts while microG is on; at most ten accounts, one line each.
        QJsonArray accounts{QStringLiteral("a@b.c\nd"), QStringLiteral("  "), QString(300, QLatin1Char('x'))};
        for (int i = 0; i < 12; ++i)
            accounts.append(QStringLiteral("n%1@gmail.com").arg(i));
        const QVariantMap gms = ManageCheck::googleServices(
            {{QStringLiteral("installed"), true}, {QStringLiteral("enabled"), false}, {QStringLiteral("push"), true},
             {QStringLiteral("accounts"), accounts}});
        QVERIFY(!gms.value(QStringLiteral("push")).toBool());
        const QStringList names = gms.value(QStringLiteral("accounts")).toStringList();
        QCOMPARE(names.size(), 10);
        QCOMPARE(names.first().size(), 254);
    }

    void matchesAnswersById() {
        QList<QJsonObject> sent;
        Manage             manage([&](const QJsonObject &m) { sent.append(m); });
        bool               answered = false;
        manage.request(QStringLiteral("info"), {}, [&](bool ok, const QJsonObject &r) {
            answered = true;
            QVERIFY(!ok);
            QCOMPARE(r.value(QStringLiteral("error")).toString(), QStringLiteral("Android isn't running"));
        });
        QVERIFY(answered);
        QVERIFY(sent.isEmpty());

        manage.setConnected(true);
        int okCount = 0, failed = 0;
        manage.request(QStringLiteral("stop"), {{QStringLiteral("pkg"), QStringLiteral("a.b")}},
                       [&](bool ok, const QJsonObject &) { ok ? ++okCount : ++failed; });
        manage.request(QStringLiteral("cache"), {{QStringLiteral("pkg"), QStringLiteral("a.b")}},
                       [&](bool ok, const QJsonObject &) { ok ? ++okCount : ++failed; });
        QCOMPARE(sent.size(), 2);
        QCOMPARE(sent[0].value(QStringLiteral("t")).toString(), QStringLiteral("manage"));
        QCOMPARE(sent[0].value(QStringLiteral("op")).toString(), QStringLiteral("stop"));
        QVERIFY(sent[0].value(QStringLiteral("id")) != sent[1].value(QStringLiteral("id")));
        manage.handle(QStringLiteral("managed"), {{QStringLiteral("id"), QStringLiteral("ffff")}, {QStringLiteral("ok"), true}});
        manage.handle(QStringLiteral("managed"), {{QStringLiteral("id"), sent[1].value(QStringLiteral("id"))}, {QStringLiteral("ok"), true}});
        QCOMPARE(okCount, 1);
        manage.handle(QStringLiteral("managed"), {{QStringLiteral("id"), sent[1].value(QStringLiteral("id"))}, {QStringLiteral("ok"), true}});
        QCOMPARE(okCount, 1);
        manage.setConnected(false);
        QCOMPARE(failed, 1);
    }

    // Over a bus: who may call what, what reaches Android, what comes back.
    void dbusApi() {
        if (m_bus.state() != QProcess::Running)
            QSKIP("no dbus-daemon");
        QDBusConnection bus = QDBusConnection::sessionBus();
        FakeShell       shell;
        QVERIFY(bus.registerObject(QStringLiteral("/org/marathonos/Shell/Navigation"), &shell,
                                   QDBusConnection::ExportAllSlots));
        QVERIFY(bus.registerService(QStringLiteral("org.marathonos.Shell")));

        QTemporaryDir dir;
        QStringList   unitCalls;
        Runtime runtime({QString(), dir.filePath(QStringLiteral("settings.json")), dir.path()},
                        [&](const QString &m) { unitCalls.append(m); });
        // Android: answers every request; "app" with a whole app, "clear" with a failure.
        QList<QJsonObject> sent;
        Manage            *managePtr = nullptr;
        Manage manage([&](const QJsonObject &m) {
            sent.append(m);
            QJsonObject r{{QStringLiteral("id"), m.value(QStringLiteral("id"))}, {QStringLiteral("ok"), true}};
            const QString op = m.value(QStringLiteral("op")).toString();
            if (op == QLatin1String("app")) {
                r.insert(QStringLiteral("pkg"), m.value(QStringLiteral("pkg")));
                r.insert(QStringLiteral("label"), QStringLiteral("F-Droid"));
                r.insert(QStringLiteral("dataBytes"), 4096);
                r.insert(QStringLiteral("perms"),
                         QJsonArray{QJsonObject{{QStringLiteral("name"), QStringLiteral("android.permission.BLUETOOTH_SCAN")},
                                                {QStringLiteral("granted"), true}}});
            } else if (op == QLatin1String("perm_apps") || op == QLatin1String("notif_apps")) {
                r.insert(QStringLiteral("apps"), QJsonArray{QJsonObject{{QStringLiteral("pkg"), QStringLiteral("org.fdroid.fdroid")},
                                                                        {QStringLiteral("label"), QStringLiteral("F-Droid")},
                                                                        {QStringLiteral("granted"), true}}});
            } else if (op == QLatin1String("special")) {
                r.insert(QStringLiteral("apps"), QJsonArray{QJsonObject{{QStringLiteral("pkg"), QStringLiteral("org.fdroid.fdroid")},
                                                                        {QStringLiteral("label"), QStringLiteral("F-Droid")},
                                                                        {QStringLiteral("granted"), true}}});
            } else if (op == QLatin1String("gms")) {
                r.insert(QStringLiteral("installed"), true);
                r.insert(QStringLiteral("enabled"), true);
                r.insert(QStringLiteral("push"), true);
                r.insert(QStringLiteral("accounts"), QJsonArray{QStringLiteral("someone@gmail.com")});
            } else if (op == QLatin1String("clear")) {
                r.insert(QStringLiteral("ok"), false);
                r.insert(QStringLiteral("error"), QStringLiteral("no such app"));
            }
            QMetaObject::invokeMethod(managePtr, [managePtr, r] { managePtr->handle(QStringLiteral("managed"), r); },
                                      Qt::QueuedConnection);
        });
        managePtr = &manage;
        manage.setConnected(true);

        const QImage icon(QSize(48, 48), QImage::Format_ARGB32);
        QVERIFY(icon.save(dir.filePath(QStringLiteral("org.fdroid.fdroid.png")), "PNG"));

        QObject     root;
        Callers     callers(bus);
        SettingsApi api(&root, bus, &callers, &manage, &runtime, {dir.path()});
        QSignalSpy  appChanged(&api, &SettingsApi::AppChanged);
        QVERIFY(bus.registerObject(QStringLiteral("/org/marathonos/Deckard"), &root, QDBusConnection::ExportAdaptors));
        QVERIFY(bus.registerService(QStringLiteral("org.marathonos.DeckardTest")));
        QTest::qWait(50);

        // Anyone may read the runtime and icons.
        shell.appId = QStringLiteral("notes");
        QDBusMessage r = call(QStringLiteral("GetRuntime"));
        QCOMPARE(r.type(), QDBusMessage::ReplyMessage);
        QCOMPARE(map(r.arguments().value(0)).value(QStringLiteral("state")).toString(), QStringLiteral("stopped"));
        r = call(QStringLiteral("AppIcon"), {QStringLiteral("org.fdroid.fdroid")});
        QVERIFY(r.arguments().value(0).toString().startsWith(QStringLiteral("data:image/png;base64,")));
        QVERIFY(call(QStringLiteral("AppIcon"), {QStringLiteral("../org.fdroid.fdroid")}).arguments().value(0).toString().isEmpty());

        // Only Settings may change anything, or read an app's details.
        for (const auto &[method, args] : QList<QPair<QString, QVariantList>>{
                 {QStringLiteral("Stop"), {}},
                 {QStringLiteral("SetStartAtLogin"), {true}},
                 {QStringLiteral("GetApp"), {QStringLiteral("org.fdroid.fdroid")}},
                 {QStringLiteral("ClearData"), {QStringLiteral("org.fdroid.fdroid")}},
                 {QStringLiteral("GetGoogleServices"), {}},
                 {QStringLiteral("SetGoogleServices"), {QStringLiteral("push"), false}},
                 {QStringLiteral("OpenGoogleServices"), {QStringLiteral("account")}},
                 {QStringLiteral("ResetAndroid"), {}}}) {
            r = call(method, args);
            QCOMPARE(r.type(), QDBusMessage::ErrorMessage);
            QCOMPARE(r.errorName(), QStringLiteral("org.freedesktop.DBus.Error.AccessDenied"));
        }
        QVERIFY(unitCalls.isEmpty());
        QVERIFY(sent.isEmpty());
        QVERIFY(!runtime.startAtLoginIsSet());

        shell.appId = QStringLiteral("settings");
        r = call(QStringLiteral("Stop"));
        QCOMPARE(r.type(), QDBusMessage::ReplyMessage);
        QCOMPARE(unitCalls, QStringList{QStringLiteral("StopUnit")});
        QCOMPARE(call(QStringLiteral("SetStartAtLogin"), {true}).type(), QDBusMessage::ReplyMessage);
        QVERIFY(runtime.startAtLoginIsSet() && runtime.startAtLogin());

        // Arguments are checked before anything reaches Android.
        r = call(QStringLiteral("GetApp"), {QStringLiteral("not a package")});
        QCOMPARE(r.errorName(), QStringLiteral("org.freedesktop.DBus.Error.InvalidArgs"));
        r = call(QStringLiteral("SetPermission"), {QStringLiteral("org.fdroid.fdroid"), QStringLiteral("android.permission.INSTALL_PACKAGES"), true});
        QCOMPARE(r.errorName(), QStringLiteral("org.freedesktop.DBus.Error.InvalidArgs"));
        r = call(QStringLiteral("SetBattery"), {QStringLiteral("org.fdroid.fdroid"), QStringLiteral("whatever")});
        QCOMPARE(r.errorName(), QStringLiteral("org.freedesktop.DBus.Error.InvalidArgs"));
        r = call(QStringLiteral("GetSpecialAccess"), {QStringLiteral("root")});
        QCOMPARE(r.errorName(), QStringLiteral("org.freedesktop.DBus.Error.InvalidArgs"));
        QVERIFY(sent.isEmpty());

        r = call(QStringLiteral("GetApp"), {QStringLiteral("org.fdroid.fdroid")});
        QCOMPARE(r.type(), QDBusMessage::ReplyMessage);
        const QVariantMap app = map(r.arguments().value(0));
        QCOMPARE(app.value(QStringLiteral("label")).toString(), QStringLiteral("F-Droid"));
        QCOMPARE(app.value(QStringLiteral("dataBytes")).toLongLong(), 4096);
        QCOMPARE(sent.last().value(QStringLiteral("op")).toString(), QStringLiteral("app"));
        QCOMPARE(appChanged.size(), 0);

        // Marathon's names in, Android's permissions out.
        r = call(QStringLiteral("SetPermission"), {QStringLiteral("org.fdroid.fdroid"), QStringLiteral("bluetooth"), false});
        QCOMPARE(r.type(), QDBusMessage::ReplyMessage);
        QCOMPARE(sent.last().value(QStringLiteral("op")).toString(), QStringLiteral("perm"));
        QVERIFY(sent.last().value(QStringLiteral("permissions")).toArray().contains(QStringLiteral("android.permission.BLUETOOTH_SCAN")));
        QCOMPARE(sent.last().value(QStringLiteral("granted")).toBool(), false);
        QCOMPARE(appChanged.size(), 1);
        QCOMPARE(appChanged[0][0].toString(), QStringLiteral("org.fdroid.fdroid"));
        r = call(QStringLiteral("SetPermission"), {QStringLiteral("org.fdroid.fdroid"), QStringLiteral("notifications"), true});
        QCOMPARE(sent.last().value(QStringLiteral("op")).toString(), QStringLiteral("notifications"));
        QCOMPARE(sent.last().value(QStringLiteral("enabled")).toBool(), true);
        r = call(QStringLiteral("SetPermission"), {QStringLiteral("org.fdroid.fdroid"), QStringLiteral("nearby"), true});
        QCOMPARE(r.errorName(), QStringLiteral("org.freedesktop.DBus.Error.InvalidArgs"));

        r = call(QStringLiteral("GetPermissionApps"), {QStringLiteral("camera")});
        QCOMPARE(r.type(), QDBusMessage::ReplyMessage);
        QCOMPARE(r.signature(), QStringLiteral("aa{sv}"));
        QCOMPARE(sent.last().value(QStringLiteral("op")).toString(), QStringLiteral("perm_apps"));
        QCOMPARE(sent.last().value(QStringLiteral("permissions")).toArray(), QJsonArray{QStringLiteral("android.permission.CAMERA")});
        r = call(QStringLiteral("GetPermissionApps"), {QStringLiteral("notifications")});
        QCOMPARE(sent.last().value(QStringLiteral("op")).toString(), QStringLiteral("notif_apps"));
        QCOMPARE(call(QStringLiteral("GetPermissionApps"), {QStringLiteral("system")}).errorName(),
                 QStringLiteral("org.freedesktop.DBus.Error.InvalidArgs"));

        r = call(QStringLiteral("GetSpecialAccess"), {QStringLiteral("install_apps")});
        QCOMPARE(r.type(), QDBusMessage::ReplyMessage);
        QCOMPARE(r.signature(), QStringLiteral("aa{sv}"));
        QCOMPARE(sent.last().value(QStringLiteral("kind")).toString(), QStringLiteral("install_apps"));

        r = call(QStringLiteral("ClearData"), {QStringLiteral("org.fdroid.fdroid")});
        QCOMPARE(r.errorName(), QStringLiteral("org.marathonos.Deckard1.Error.Failed"));
        QCOMPARE(r.errorMessage(), QStringLiteral("no such app"));
        QCOMPARE(appChanged.size(), 2);

        // microG: two settings and two of its pages, nothing else.
        const int before = appChanged.size();
        r = call(QStringLiteral("GetGoogleServices"));
        QCOMPARE(r.type(), QDBusMessage::ReplyMessage);
        const QVariantMap gms = map(r.arguments().value(0));
        QVERIFY(gms.value(QStringLiteral("enabled")).toBool());
        QCOMPARE(gms.value(QStringLiteral("accounts")).toStringList(), QStringList{QStringLiteral("someone@gmail.com")});
        QCOMPARE(sent.last().value(QStringLiteral("op")).toString(), QStringLiteral("gms"));
        QCOMPARE(call(QStringLiteral("SetGoogleServices"), {QStringLiteral("push"), false}).type(), QDBusMessage::ReplyMessage);
        QCOMPARE(sent.last().value(QStringLiteral("op")).toString(), QStringLiteral("gms_set"));
        QCOMPARE(sent.last().value(QStringLiteral("key")).toString(), QStringLiteral("push"));
        QCOMPARE(sent.last().value(QStringLiteral("enabled")).toBool(), false);
        QCOMPARE(call(QStringLiteral("OpenGoogleServices"), {QStringLiteral("selfcheck")}).type(), QDBusMessage::ReplyMessage);
        QCOMPARE(sent.last().value(QStringLiteral("page")).toString(), QStringLiteral("selfcheck"));
        const qsizetype count = sent.size();
        QCOMPARE(call(QStringLiteral("SetGoogleServices"), {QStringLiteral("droidguard"), true}).errorName(),
                 QStringLiteral("org.freedesktop.DBus.Error.InvalidArgs"));
        QCOMPARE(call(QStringLiteral("OpenGoogleServices"), {QStringLiteral("settings")}).errorName(),
                 QStringLiteral("org.freedesktop.DBus.Error.InvalidArgs"));
        QCOMPARE(sent.size(), count);
        QCOMPARE(appChanged.size(), before);

        manage.setConnected(false);
        r = call(QStringLiteral("ForceStop"), {QStringLiteral("org.fdroid.fdroid")});
        QCOMPARE(r.errorName(), QStringLiteral("org.marathonos.Deckard1.Error.Failed"));
    }
};

QTEST_GUILESS_MAIN(TestSettingsApi)
#include "test_settingsapi.moc"
