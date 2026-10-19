#include "permissionprompts.h"

#include <QDBusConnection>
#include <QDBusContext>
#include <QDBusMessage>
#include <QJsonArray>
#include <QProcess>
#include <QSignalSpy>
#include <QStandardPaths>
#include <QTest>

// The shell's org.marathonos.Shell.Permissions1: holds each prompt until
// the test answers it.
// On a connection of its own: a call within one connection can't wait.
QDBusConnection shellBus() {
    return QDBusConnection::connectToBus(QDBusConnection::SessionBus, QStringLiteral("fake-shell"));
}

class FakePermissions : public QObject, protected QDBusContext {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.marathonos.Shell.Permissions1")

  public:
    struct Prompt {
        QString      appId;
        QString      permission;
        QVariantMap  options;
        QDBusMessage call;
    };
    QList<Prompt> prompts;
    QStringList   cancelled;

    void answer(int i, const QString &answer, bool precise = false) {
        shellBus().send(prompts[i].call.createReply(QVariant(QVariantMap{
            {QStringLiteral("answer"), answer}, {QStringLiteral("precise"), precise}})));
    }

  public Q_SLOTS:
    Q_SCRIPTABLE QVariantMap PromptAndroid(const QString &appId, const QString &permission,
                                           const QVariantMap &options) {
        setDelayedReply(true);
        prompts.append({appId, permission, options, message()});
        return {};
    }
    Q_SCRIPTABLE void CancelAndroidPrompt(const QString &request) {
        cancelled.append(request);
        for (const Prompt &p : std::as_const(prompts))
            if (p.options.value(QStringLiteral("request")) == request)
                shellBus().send(p.call.createReply(QVariant(QVariantMap{
                    {QStringLiteral("answer"), QStringLiteral("cancel")}, {QStringLiteral("precise"), false}})));
    }
};

class TestPermissionPrompts : public QObject {
    Q_OBJECT

    QProcess         m_bus;
    FakePermissions *m_shell = nullptr;

    static QJsonObject ask(const QString &id, const QStringList &perms, const QJsonObject &extra = {}) {
        QJsonObject m{{QStringLiteral("t"), QStringLiteral("perm_ask")},
                      {QStringLiteral("id"), id},
                      {QStringLiteral("pkg"), QStringLiteral("me.hackerchick.catima")},
                      {QStringLiteral("perms"), QJsonArray::fromStringList(perms)},
                      {QStringLiteral("kind"), QStringLiteral("foreground")}};
        for (auto it = extra.begin(); it != extra.end(); ++it)
            m.insert(it.key(), it.value());
        return m;
    }

    static QString perm(const char *name) {
        return QStringLiteral("android.permission.") + QLatin1String(name);
    }

  private Q_SLOTS:
    void initTestCase() {
        const QString daemon = QStandardPaths::findExecutable(QStringLiteral("dbus-daemon"));
        if (daemon.isEmpty())
            QSKIP("no dbus-daemon");
        m_bus.start(daemon, {QStringLiteral("--session"), QStringLiteral("--nofork"),
                             QStringLiteral("--print-address")});
        QVERIFY(m_bus.waitForReadyRead(5000));
        qputenv("DBUS_SESSION_BUS_ADDRESS", m_bus.readLine().trimmed());
    }

    void init() {
        m_shell = new FakePermissions;
        QDBusConnection bus = shellBus();
        QVERIFY(bus.registerObject(QStringLiteral("/org/marathonos/Shell/Permissions"), m_shell,
                                   QDBusConnection::ExportScriptableSlots));
        QVERIFY(bus.registerService(QStringLiteral("org.marathonos.Shell")));
    }

    void cleanup() {
        QDBusConnection bus = shellBus();
        bus.unregisterObject(QStringLiteral("/org/marathonos/Shell/Permissions"));
        bus.unregisterService(QStringLiteral("org.marathonos.Shell"));
        delete m_shell;
        m_shell = nullptr;
    }

    void cleanupTestCase() {
        m_bus.kill();
        m_bus.waitForFinished();
    }

    void aRequestBecomesTheShellsPrompt() {
        QList<QJsonObject> sent;
        PermissionPrompts  prompts(QDBusConnection::sessionBus(), [&](const QJsonObject &m) { sent.append(m); });
        prompts.handle(QStringLiteral("perm_ask"),
                       ask(QStringLiteral("0123456789abcdef"), {perm("CAMERA")}, {{QStringLiteral("once"), true}}));
        QTRY_COMPARE(m_shell->prompts.size(), 1);
        const FakePermissions::Prompt p = m_shell->prompts[0];
        QCOMPARE(p.appId, QStringLiteral("waydroid.me.hackerchick.catima"));
        QCOMPARE(p.permission, QStringLiteral("camera"));
        QCOMPARE(p.options.value(QStringLiteral("request")).toString(), QStringLiteral("0123456789abcdef"));
        QCOMPARE(p.options.value(QStringLiteral("kind")).toString(), QStringLiteral("foreground"));
        QCOMPARE(p.options.value(QStringLiteral("once")).toBool(), true);
        QCOMPARE(p.options.value(QStringLiteral("accuracy")).toString(), QString());
        QVERIFY(sent.isEmpty());

        m_shell->answer(0, QStringLiteral("once"));
        QTRY_COMPARE(sent.size(), 1);
        QCOMPARE(sent[0].value(QStringLiteral("t")).toString(), QStringLiteral("perm_answer"));
        QCOMPARE(sent[0].value(QStringLiteral("id")).toString(), QStringLiteral("0123456789abcdef"));
        QCOMPARE(sent[0].value(QStringLiteral("answer")).toString(), QStringLiteral("once"));
        QCOMPARE(sent[0].value(QStringLiteral("precise")).toBool(), false);
    }

    void locationKeepsItsChoices() {
        QList<QJsonObject> sent;
        PermissionPrompts  prompts(QDBusConnection::sessionBus(), [&](const QJsonObject &m) { sent.append(m); });
        prompts.handle(QStringLiteral("perm_ask"),
                       ask(QStringLiteral("00000000000000aa"), {perm("ACCESS_FINE_LOCATION"), perm("ACCESS_COARSE_LOCATION")},
                           {{QStringLiteral("once"), true},
                            {QStringLiteral("accuracy"), QStringLiteral("choice")},
                            {QStringLiteral("preciseDefault"), true}}));
        QTRY_COMPARE(m_shell->prompts.size(), 1);
        QCOMPARE(m_shell->prompts[0].permission, QStringLiteral("location"));
        QCOMPARE(m_shell->prompts[0].options.value(QStringLiteral("accuracy")).toString(), QStringLiteral("choice"));
        QVERIFY(m_shell->prompts[0].options.value(QStringLiteral("preciseDefault")).toBool());
        m_shell->answer(0, QStringLiteral("allow"), true);
        QTRY_COMPARE(sent.size(), 1);
        QCOMPARE(sent[0].value(QStringLiteral("answer")).toString(), QStringLiteral("allow"));
        QVERIFY(sent[0].value(QStringLiteral("precise")).toBool());

        prompts.handle(QStringLiteral("perm_ask"),
                       ask(QStringLiteral("00000000000000ab"), {perm("ACCESS_BACKGROUND_LOCATION")},
                           {{QStringLiteral("kind"), QStringLiteral("background")}}));
        QTRY_COMPARE(m_shell->prompts.size(), 2);
        QCOMPARE(m_shell->prompts[1].options.value(QStringLiteral("kind")).toString(), QStringLiteral("background"));
    }

    // Requests Marathon can't draw go back to Android's own dialog.
    void unsupportedRequestsAreHandedBack() {
        QList<QJsonObject> sent;
        PermissionPrompts  prompts(QDBusConnection::sessionBus(), [&](const QJsonObject &m) { sent.append(m); });
        const QList<QJsonObject> bad = {
            ask(QStringLiteral("0000000000000001"), {QStringLiteral("com.example.permission.CUSTOM")}),
            ask(QStringLiteral("0000000000000002"), {perm("CAMERA"), perm("READ_CONTACTS")}),
            ask(QStringLiteral("0000000000000003"), {}),
            ask(QStringLiteral("0000000000000004"), {perm("CAMERA")}, {{QStringLiteral("kind"), QStringLiteral("always")}}),
            ask(QStringLiteral("0000000000000005"), {perm("CAMERA")}, {{QStringLiteral("accuracy"), QStringLiteral("fine")}}),
            ask(QStringLiteral("0000000000000006"), {perm("CAMERA")}, {{QStringLiteral("pkg"), QStringLiteral("bad pkg")}}),
            ask(QStringLiteral("0000000000000007"), {perm("CAMERA")}, {{QStringLiteral("once"), QStringLiteral("yes")}}),
        };
        for (const QJsonObject &m : bad)
            prompts.handle(QStringLiteral("perm_ask"), m);
        QCOMPARE(sent.size(), bad.size());
        for (int i = 0; i < sent.size(); ++i) {
            QCOMPARE(sent[i].value(QStringLiteral("id")).toString(), bad[i].value(QStringLiteral("id")).toString());
            QCOMPARE(sent[i].value(QStringLiteral("answer")).toString(), QStringLiteral("unsupported"));
        }
        prompts.handle(QStringLiteral("perm_ask"), ask(QStringLiteral("not-an-id"), {perm("CAMERA")}));
        QCOMPARE(sent.size(), bad.size());
        QTest::qWait(100);
        QVERIFY(m_shell->prompts.isEmpty());
    }

    void withoutTheShellAndroidAsks() {
        cleanup();
        QList<QJsonObject> sent;
        PermissionPrompts  prompts(QDBusConnection::sessionBus(), [&](const QJsonObject &m) { sent.append(m); });
        prompts.handle(QStringLiteral("perm_ask"), ask(QStringLiteral("0000000000000010"), {perm("RECORD_AUDIO")}));
        QTRY_COMPARE(sent.size(), 1);
        QCOMPARE(sent[0].value(QStringLiteral("answer")).toString(), QStringLiteral("unsupported"));
        init();
    }

    void cancelsReachTheShell() {
        QList<QJsonObject> sent;
        PermissionPrompts  prompts(QDBusConnection::sessionBus(), [&](const QJsonObject &m) { sent.append(m); });
        prompts.handle(QStringLiteral("perm_ask"), ask(QStringLiteral("0000000000000020"), {perm("CAMERA")}));
        prompts.handle(QStringLiteral("perm_ask"), ask(QStringLiteral("0000000000000021"), {perm("READ_CALENDAR")}));
        QTRY_COMPARE(m_shell->prompts.size(), 2);
        prompts.handle(QStringLiteral("perm_cancel"), {{QStringLiteral("id"), QStringLiteral("0000000000000020")}});
        prompts.handle(QStringLiteral("perm_cancel"), {{QStringLiteral("id"), QStringLiteral("00000000000000ff")}});
        QTRY_COMPARE(m_shell->cancelled, QStringList({QStringLiteral("0000000000000020")}));

        prompts.systemLost();
        QTRY_COMPARE(m_shell->cancelled.size(), 2);
        QCOMPARE(m_shell->cancelled[1], QStringLiteral("0000000000000021"));
        QTest::qWait(100);
        QVERIFY(sent.isEmpty());
    }

    void waitingIsBounded() {
        QList<QJsonObject> sent;
        PermissionPrompts  prompts(QDBusConnection::sessionBus(), [&](const QJsonObject &m) { sent.append(m); });
        for (int i = 0; i < PermissionPrompts::kMaxWaiting + 1; ++i)
            prompts.handle(QStringLiteral("perm_ask"),
                           ask(QStringLiteral("00000000000001%1").arg(i, 2, 16, QLatin1Char('0')), {perm("CAMERA")}));
        QTRY_COMPARE(m_shell->prompts.size(), PermissionPrompts::kMaxWaiting);
        QCOMPARE(sent.size(), 1);
        QCOMPARE(sent[0].value(QStringLiteral("answer")).toString(), QStringLiteral("unsupported"));
        // A repeated id is the same request.
        prompts.handle(QStringLiteral("perm_ask"), ask(QStringLiteral("0000000000000100"), {perm("CAMERA")}));
        QCOMPARE(sent.size(), 1);
        prompts.systemLost();
    }
};

QTEST_GUILESS_MAIN(TestPermissionPrompts)
#include "test_permissionprompts.moc"
