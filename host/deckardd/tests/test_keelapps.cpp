#include "keelapps.h"

#include <QCoreApplication>
#include <QDBusConnectionInterface>
#include <QDBusPendingCallWatcher>
#include <QJsonArray>
#include <QJsonDocument>
#include <QProcess>
#include <QTemporaryDir>
#include <QTest>

using namespace KeelAndroid;

namespace {

    QJsonObject sample() {
        return QJsonDocument::fromJson(R"({
            "t": "app_info", "pkg": "org.example.chat", "label": "Chat\u0007 App",
            "links": [{"s": "https", "h": "chat.example.org"}, {"s": "chat", "h": ""},
                      {"s": "file", "h": ""}, {"s": "javascript", "h": ""}, {"s": "HTTPS", "h": "CHAT.example.org"}],
            "share": true, "search": "gms",
            "shortcuts": [{"id": "compose", "label": "New message", "long": "Write a new message"},
                          {"id": "compose!", "label": "Compose again"},
                          {"id": "wipe_all", "label": "Delete all chats"},
                          {"id": "bad\u0001id", "label": "Bad"},
                          {"id": "nolabel", "label": ""}]
        })").object();
    }

    QJsonObject toolOf(const QJsonObject &manifest, const QString &action) {
        for (const QJsonValue &t : manifest.value(QStringLiteral("tools")).toArray()) {
            if (t[QStringLiteral("_meta")][QStringLiteral("org.shipwright.keel/action")].toString() == action)
                return t.toObject();
        }
        return {};
    }

} // namespace

// org.marathonos.shell's Keel Actions, as deckardd uses them.
class FakeShell : public QObject {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.shipwright.Keel.Actions")

  public:
    bool        locked = false;
    QString     front;
    QStringList running;
    QStringList launched;

  public Q_SLOTS:
    QString Invoke(const QString &action, const QString &arguments, const QVariantMap &) {
        const QJsonObject args = QJsonDocument::fromJson(arguments.toUtf8()).object();
        if (action == QLatin1String("shell.state"))
            return QString::fromUtf8(QJsonDocument(QJsonObject{{QStringLiteral("locked"), locked},
                                                               {QStringLiteral("screenOn"), true},
                                                               {QStringLiteral("foregroundApp"), front}})
                                         .toJson(QJsonDocument::Compact));
        if (action == QLatin1String("apps.running")) {
            QJsonArray list;
            for (const QString &id : running)
                list.append(QJsonObject{{QStringLiteral("id"), id}, {QStringLiteral("name"), id}});
            return QString::fromUtf8(
                QJsonDocument(QJsonObject{{QStringLiteral("result"), list}}).toJson(QJsonDocument::Compact));
        }
        if (action == QLatin1String("app.launch")) {
            launched << args.value(QStringLiteral("id")).toString();
            front = launched.last();
            return QStringLiteral("{}");
        }
        return {};
    }
};

class TestKeelApps : public QObject {
    Q_OBJECT

    QProcess m_daemon;
    QString  m_address;

    QDBusConnection bus(const QString &name) {
        return QDBusConnection::connectToBus(m_address, name);
    }

    // Invoke on name as a client of the private bus, answered or failed.
    QDBusMessage call(const QString &name, const QString &method, const QVariantList &args) {
        QDBusMessage m = QDBusMessage::createMethodCall(name, QStringLiteral("/org/shipwright/Keel/Actions"),
                                                        QStringLiteral("org.shipwright.Keel.Actions"), method);
        m.setArguments(args);
        QDBusPendingCallWatcher w(bus(QStringLiteral("client")).asyncCall(m, 20000));
        if (!QTest::qWaitFor([&w] { return w.isFinished(); }, 20000))
            return {};
        return w.reply();
    }

    static QVariantMap pilot() {
        return {{QStringLiteral("userInitiated"), true}};
    }

    static QString json(const QDBusMessage &reply) {
        return reply.arguments().value(0).toString();
    }

  private Q_SLOTS:
    void initTestCase() {
        m_daemon.start(QStringLiteral("dbus-daemon"),
                       {QStringLiteral("--session"), QStringLiteral("--nofork"), QStringLiteral("--print-address=1")});
        QVERIFY(m_daemon.waitForStarted(5000));
        QVERIFY(QTest::qWaitFor([this] { return m_daemon.canReadLine(); }, 5000));
        m_address = QString::fromUtf8(m_daemon.readLine()).trimmed();
        QVERIFY(bus(QStringLiteral("client")).isConnected());
    }

    void cleanupTestCase() {
        m_daemon.terminate();
        m_daemon.waitForFinished(3000);
    }

    void appsAreCleaned() {
        QString    error;
        const auto a = app(sample(), &error);
        QVERIFY(a);
        QCOMPARE(a->label, QStringLiteral("Chat App"));
        QCOMPARE(a->links.size(), 2);
        QCOMPARE(a->links.at(0).host, QStringLiteral("chat.example.org"));
        QCOMPARE(a->links.at(1).scheme, QStringLiteral("chat"));
        QCOMPARE(a->search, QStringLiteral("gms"));
        QCOMPARE(a->shortcuts.size(), 3);
        QCOMPARE(a->shortcuts.at(2).id, QStringLiteral("wipe_all"));

        QJsonObject own = sample();
        own.insert(QStringLiteral("pkg"), QStringLiteral("org.marathonos.deckard.bridge"));
        QVERIFY(!app(own, &error));
        own.insert(QStringLiteral("pkg"), QStringLiteral("not a package"));
        QVERIFY(!app(own, &error));

        const auto again = app(toJson(*a), &error);
        QVERIFY(again);
        QCOMPARE(toJson(*again), toJson(*a));
    }

    void manifestsAreKeelManifests() {
        QString    error;
        const auto m = manifest(*app(sample(), &error));
        QCOMPARE(m.json.value(QStringLiteral("appId")).toString(), QStringLiteral("waydroid.org.example.chat"));
        QCOMPARE(m.json.value(QStringLiteral("version")).toInt(), 1);
        QCOMPARE(m.json[QStringLiteral("context")][QStringLiteral("uri")].toString(),
                 QStringLiteral("keel://waydroid.org.example.chat/context"));
        for (const QJsonValue &t : m.json.value(QStringLiteral("tools")).toArray()) {
            const QString name = t[QStringLiteral("name")].toString();
            QVERIFY(name.startsWith(QLatin1String("waydroid.org.example.chat/")));
            QVERIFY(t[QStringLiteral("description")].toString().size() >= 20);
            QCOMPARE(t[QStringLiteral("inputSchema")][QStringLiteral("additionalProperties")].toBool(true), false);
        }

        const QJsonObject show = toolOf(m.json, QStringLiteral("app.show"));
        QVERIFY(show[QStringLiteral("inputSchema")][QStringLiteral("properties")].toObject().contains(QStringLiteral("link")));
        QVERIFY(show[QStringLiteral("description")].toString().contains(QLatin1String("https://chat.example.org")));
        QVERIFY(!toolOf(m.json, QStringLiteral("share.text")).isEmpty());
        QVERIFY(toolOf(m.json, QStringLiteral("share.text"))[QStringLiteral("annotations")][QStringLiteral("openWorldHint")].toBool());
        QVERIFY(!toolOf(m.json, QStringLiteral("app.search")).isEmpty());

        const QJsonObject compose = toolOf(m.json, QStringLiteral("shortcut.compose"));
        QCOMPARE(compose[QStringLiteral("title")].toString(), QStringLiteral("New message (Chat App)"));
        QCOMPARE(compose[QStringLiteral("_meta")][QStringLiteral("org.shipwright.keel/confirm")].toBool(), false);
        QCOMPARE(m.shortcuts.value(QStringLiteral("shortcut.compose_2")), QStringLiteral("compose!"));
        const QJsonObject wipe = toolOf(m.json, QStringLiteral("shortcut.wipe_all"));
        QVERIFY(wipe[QStringLiteral("_meta")][QStringLiteral("org.shipwright.keel/confirm")].toBool());
        QVERIFY(wipe[QStringLiteral("annotations")][QStringLiteral("destructiveHint")].toBool());

        const QJsonObject read = toolOf(m.json, QStringLiteral("screen.read"));
        QVERIFY(read[QStringLiteral("annotations")][QStringLiteral("readOnlyHint")].toBool());
        QVERIFY(read[QStringLiteral("_meta")][QStringLiteral("org.shipwright.keel/untrusted")].toBool());
        QVERIFY(read.contains(QStringLiteral("outputSchema")));
        QVERIFY(toolOf(m.json, QStringLiteral("ui.tap"))[QStringLiteral("_meta")][QStringLiteral("org.shipwright.keel/confirm")].toBool());
        QVERIFY(toolOf(m.json, QStringLiteral("ui.type"))[QStringLiteral("_meta")][QStringLiteral("org.shipwright.keel/confirm")].toBool());

        App bare;
        bare.pkg   = QStringLiteral("org.example.bare");
        bare.label = QStringLiteral("Bare");
        const auto b = manifest(bare);
        QVERIFY(toolOf(b.json, QStringLiteral("share.text")).isEmpty());
        QVERIFY(toolOf(b.json, QStringLiteral("app.show"))[QStringLiteral("inputSchema")][QStringLiteral("properties")]
                    .toObject()
                    .isEmpty());

        App longName = bare;
        longName.pkg = QStringLiteral("org.") + QString(110, u'x');
        for (const QJsonValue &t : manifest(longName).json.value(QStringLiteral("tools")).toArray())
            QVERIFY(t[QStringLiteral("name")].toString().size() + 1 <= 128);
    }

    void shortcutNames() {
        QCOMPARE(shortcutAction(QStringLiteral("New-Chat"), {}), QStringLiteral("shortcut.new_chat"));
        QCOMPARE(shortcutAction(QStringLiteral("123"), {}), QStringLiteral("shortcut.s123"));
        QCOMPARE(shortcutAction(QStringLiteral("!!!"), {}), QStringLiteral("shortcut.s"));
        QCOMPARE(shortcutAction(QStringLiteral("a"), {QStringLiteral("shortcut.a")}), QStringLiteral("shortcut.a_2"));
        QVERIFY(needsConfirmation(QStringLiteral("Call Mum")));
        QVERIFY(needsConfirmation(QStringLiteral("Start recording")) == false);
        QVERIFY(needsConfirmation(QStringLiteral("Record")));
        QVERIFY(!needsConfirmation(QStringLiteral("Navigate home")));
    }

    void links() {
        QString why;
        QVERIFY(linkAllowed(QStringLiteral("https://chat.example.org/u/1"), &why));
        QVERIFY(linkAllowed(QStringLiteral("chat://room/42"), &why));
        QVERIFY(!linkAllowed(QStringLiteral("file:///data/data/x"), &why));
        QVERIFY(!linkAllowed(QStringLiteral("content://media/1"), &why));
        QVERIFY(!linkAllowed(QStringLiteral("intent://x#Intent;component=a/b;end"), &why));
        QVERIFY(!linkAllowed(QStringLiteral("https:///nohost"), &why));
        QVERIFY(!linkAllowed(QStringLiteral("no scheme"), &why));
        QVERIFY(!linkAllowed(QStringLiteral("https://a.b/") + QChar(0x202e), &why));
    }

    void screens() {
        const QJsonObject m = QJsonDocument::fromJson(R"({
            "t": "screen", "pkg": "org.example.chat", "snap": 3, "title": "Anna",
            "nodes": [
                {"i": 0, "d": 0, "c": "android.widget.TextView", "t": "Anna\nonline", "f": 1024},
                {"i": 1, "d": 1, "c": "androidx.recyclerview.widget.RecyclerView", "f": 256},
                {"i": 2, "d": 2, "c": "android.widget.TextView", "t": "See you at 5"},
                {"i": 3, "d": 1, "c": "android.widget.EditText", "t": "hello", "h": "Message", "v": "org.example.chat:id/input", "f": 132},
                {"i": 4, "d": 1, "c": "android.widget.EditText", "t": "hunter2", "f": 516},
                {"i": 5, "d": 1, "c": "android.widget.ImageButton", "cd": "Send", "v": "org.example.chat:id/send", "f": 65},
                {"i": 6, "d": 1, "c": "android.widget.Switch", "t": "Mute", "f": 25},
                {"i": -1, "d": 0, "c": "x", "t": "dropped"}
            ]
        })").object();
        QString    error;
        const auto s = screen(m, &error);
        QVERIFY(s);
        QCOMPARE(s->snapshot, 3);
        QCOMPARE(s->nodes.size(), 7);
        QVERIFY(s->nodes.at(4).text.isEmpty());

        bool          truncated = true;
        const QString text      = render(*s, 16000, &truncated);
        QVERIFY(!truncated);
        const QStringList lines = text.split(u'\n', Qt::SkipEmptyParts);
        QCOMPARE(lines.at(0), QStringLiteral("[3.0] text \"Anna\\nonline\" {heading}"));
        QCOMPARE(lines.at(1), QStringLiteral("  [3.1] list {scrolls}"));
        QCOMPARE(lines.at(2), QStringLiteral("    [3.2] text \"See you at 5\""));
        QCOMPARE(lines.at(3), QStringLiteral("  [3.3] field \"hello\" hint \"Message\" #input {focused}"));
        QCOMPARE(lines.at(4), QStringLiteral("  [3.4] field password (hidden)"));
        QCOMPARE(lines.at(5), QStringLiteral("  [3.5] button (Send) #send {disabled}"));
        QCOMPARE(lines.at(6), QStringLiteral("  [3.6] switch \"Mute\" {checked}"));
        QVERIFY(!text.contains(QLatin1String("hunter2")));

        render(*s, 60, &truncated);
        QVERIFY(truncated);

        QJsonObject bad = m;
        bad.insert(QStringLiteral("snap"), 0);
        QVERIFY(!screen(bad, &error));

        qint64 snap  = 0;
        int    index = 0;
        QVERIFY(parseNode(QStringLiteral("3.17"), &snap, &index));
        QCOMPARE(snap, 3);
        QCOMPARE(index, 17);
        QVERIFY(!parseNode(QStringLiteral("17"), &snap, &index));
        QVERIFY(!parseNode(QStringLiteral("0.1"), &snap, &index));
        QVERIFY(!parseNode(QStringLiteral("3.17; rm"), &snap, &index));

        QVERIFY(isExternalHost(QByteArray("keel-mcp\0--socket\0--external\0", 30)));
        QVERIFY(!isExternalHost(QByteArray("keel-mcp\0--stdio\0", 17)));
    }

    void overDBus() {
        FakeShell       fakeShell;
        QDBusConnection shellBus = bus(QStringLiteral("shell"));
        QVERIFY(shellBus.registerObject(QStringLiteral("/org/shipwright/Keel/Actions"), &fakeShell,
                                        QDBusConnection::ExportAllSlots));
        QVERIFY(shellBus.registerService(QStringLiteral("org.marathonos.shell")));

        QTemporaryDir     dir;
        QList<QJsonObject> sent;
        const QString      statePath = dir.filePath(QStringLiteral("state/apps.json"));
        auto               apps      = std::make_unique<KeelApps>(
            [&sent](const QJsonObject &m) { sent << m; },
            [this](const QString &name) { return bus(name); }, bus(QStringLiteral("deckard")),
            QStringList{QCoreApplication::applicationFilePath()}, statePath);
        apps->setConnected(true);
        apps->handle(QStringLiteral("app_info"), sample());
        QCOMPARE(apps->packages(), QStringList{QStringLiteral("org.example.chat")});

        const QString name = QStringLiteral("waydroid.org.example.chat");
        QDBusConnectionInterface *daemon = bus(QStringLiteral("client")).interface();
        QVERIFY(daemon->isServiceRegistered(name));

        // keel-mcp's probe: Describe answers the manifest of the name asked.
        QDBusMessage reply = call(name, QStringLiteral("Describe"), {});
        QCOMPARE(QJsonDocument::fromJson(json(reply).toUtf8()).object().value(QStringLiteral("appId")).toString(), name);

        // app.show without a link is the shell's launch.
        reply = call(name, QStringLiteral("Invoke"),
                     {QStringLiteral("app.show"), QStringLiteral("{}"), QVariantMap()});
        QCOMPARE(reply.type(), QDBusMessage::ReplyMessage);
        QCOMPARE(fakeShell.launched, QStringList{name});

        reply = call(name, QStringLiteral("Invoke"),
                     {QStringLiteral("app.show"), QStringLiteral("{\"bogus\":\"x\"}"), QVariantMap()});
        QCOMPARE(reply.errorName(), QStringLiteral("org.shipwright.Keel.Actions.Error.InvalidArguments"));
        reply = call(name, QStringLiteral("Invoke"),
                     {QStringLiteral("app.show"), QStringLiteral("{\"link\":\"file:///etc/passwd\"}"), QVariantMap()});
        QCOMPARE(reply.errorName(), QStringLiteral("org.shipwright.Keel.Actions.Error.InvalidArguments"));
        reply = call(name, QStringLiteral("Invoke"), {QStringLiteral("nope.nope"), QStringLiteral("{}"), QVariantMap()});
        QCOMPARE(reply.errorName(), QStringLiteral("org.shipwright.Keel.Actions.Error.UnknownAction"));

        // A shortcut: Android starts it, then the shell brings the window forward.
        fakeShell.launched.clear();
        sent.clear();
        QDBusPendingCallWatcher shortcut(bus(QStringLiteral("client")).asyncCall(
            [&] {
                QDBusMessage m = QDBusMessage::createMethodCall(name, QStringLiteral("/org/shipwright/Keel/Actions"),
                                                                QStringLiteral("org.shipwright.Keel.Actions"),
                                                                QStringLiteral("Invoke"));
                m << QStringLiteral("shortcut.compose") << QStringLiteral("{}") << QVariantMap();
                return m;
            }(),
            20000));
        QTRY_COMPARE(sent.size(), 1);
        QCOMPARE(sent.at(0).value(QStringLiteral("t")).toString(), QStringLiteral("launch"));
        QCOMPARE(sent.at(0).value(QStringLiteral("kind")).toString(), QStringLiteral("shortcut"));
        QCOMPARE(sent.at(0).value(QStringLiteral("id")).toString(), QStringLiteral("compose"));
        QVERIFY(!shortcut.isFinished());
        fakeShell.running = QStringList{name};
        apps->handle(QStringLiteral("launched"), {{QStringLiteral("t"), QStringLiteral("launched")},
                                                  {QStringLiteral("req"), sent.at(0).value(QStringLiteral("req"))},
                                                  {QStringLiteral("ok"), true}});
        QTRY_VERIFY(shortcut.isFinished());
        QCOMPARE(shortcut.reply().type(), QDBusMessage::ReplyMessage);
        QCOMPARE(fakeShell.launched, QStringList{name});

        // Android refusing to start it is the call's failure.
        sent.clear();
        QDBusPendingCallWatcher share(bus(QStringLiteral("client")).asyncCall(
            [&] {
                QDBusMessage m = QDBusMessage::createMethodCall(name, QStringLiteral("/org/shipwright/Keel/Actions"),
                                                                QStringLiteral("org.shipwright.Keel.Actions"),
                                                                QStringLiteral("Invoke"));
                m << QStringLiteral("share.text") << QStringLiteral("{\"text\":\"hi\"}") << QVariantMap();
                return m;
            }(),
            20000));
        QTRY_COMPARE(sent.size(), 1);
        QCOMPARE(sent.at(0).value(QStringLiteral("kind")).toString(), QStringLiteral("send"));
        QCOMPARE(sent.at(0).value(QStringLiteral("text")).toString(), QStringLiteral("hi"));
        apps->handle(QStringLiteral("launched"), {{QStringLiteral("req"), sent.at(0).value(QStringLiteral("req"))},
                                                  {QStringLiteral("ok"), false},
                                                  {QStringLiteral("error"), QStringLiteral("no activity")}});
        QTRY_VERIFY(share.isFinished());
        QCOMPARE(share.reply().errorName(), QStringLiteral("org.shipwright.Keel.Actions.Error.Failed"));

        // The screen of the app in front, its ids, and a tap on one.
        fakeShell.front = QStringLiteral("browser");
        reply = call(name, QStringLiteral("Invoke"), {QStringLiteral("screen.read"), QStringLiteral("{}"), pilot()});
        QCOMPARE(reply.errorName(), QStringLiteral("org.shipwright.Keel.Actions.Error.NotAvailable"));
        fakeShell.front = name;
        sent.clear();
        QDBusPendingCallWatcher read(bus(QStringLiteral("client")).asyncCall(
            [&] {
                QDBusMessage m = QDBusMessage::createMethodCall(name, QStringLiteral("/org/shipwright/Keel/Actions"),
                                                                QStringLiteral("org.shipwright.Keel.Actions"),
                                                                QStringLiteral("Invoke"));
                m << QStringLiteral("screen.read") << QStringLiteral("{}") << QVariantMap();
                return m;
            }(),
            20000));
        QTRY_COMPARE(sent.size(), 1);
        QCOMPARE(sent.at(0).value(QStringLiteral("t")).toString(), QStringLiteral("screen_read"));
        QCOMPARE(sent.at(0).value(QStringLiteral("pkg")).toString(), QStringLiteral("org.example.chat"));
        apps->handle(QStringLiteral("screen"),
                     {{QStringLiteral("req"), sent.at(0).value(QStringLiteral("req"))},
                      {QStringLiteral("ok"), true},
                      {QStringLiteral("pkg"), QStringLiteral("org.example.chat")},
                      {QStringLiteral("snap"), 7},
                      {QStringLiteral("nodes"),
                       QJsonArray{QJsonObject{{QStringLiteral("i"), 2},
                                              {QStringLiteral("c"), QStringLiteral("android.widget.Button")},
                                              {QStringLiteral("t"), QStringLiteral("Send")},
                                              {QStringLiteral("f"), 1}}}}});
        QTRY_VERIFY(read.isFinished());
        const QJsonObject result = QJsonDocument::fromJson(json(read.reply()).toUtf8()).object();
        QCOMPARE(result.value(QStringLiteral("screen")).toString(), QStringLiteral("[7.2] button \"Send\"\n"));
        QCOMPARE(result.value(QStringLiteral("snapshot")).toInt(), 7);

        reply = call(name, QStringLiteral("Invoke"),
                     {QStringLiteral("ui.tap"), QStringLiteral("{\"node\":\"7.2\"}"), QVariantMap()});
        QCOMPARE(reply.errorName(), QStringLiteral("org.shipwright.Keel.Actions.Error.NotAvailable"));
        reply = call(name, QStringLiteral("Invoke"), {QStringLiteral("ui.tap"), QStringLiteral("{\"node\":\"6.2\"}"), pilot()});
        QCOMPARE(reply.errorName(), QStringLiteral("org.shipwright.Keel.Actions.Error.InvalidArguments"));
        sent.clear();
        QDBusPendingCallWatcher tap(bus(QStringLiteral("client")).asyncCall(
            [&] {
                QDBusMessage m = QDBusMessage::createMethodCall(name, QStringLiteral("/org/shipwright/Keel/Actions"),
                                                                QStringLiteral("org.shipwright.Keel.Actions"),
                                                                QStringLiteral("Invoke"));
                m << QStringLiteral("ui.tap") << QStringLiteral("{\"node\":\"7.2\"}") << pilot();
                return m;
            }(),
            20000));
        QTRY_COMPARE(sent.size(), 1);
        QCOMPARE(sent.at(0).value(QStringLiteral("t")).toString(), QStringLiteral("ui_act"));
        QCOMPARE(sent.at(0).value(QStringLiteral("act")).toString(), QStringLiteral("click"));
        QCOMPARE(sent.at(0).value(QStringLiteral("node")).toInt(), 2);
        QCOMPARE(sent.at(0).value(QStringLiteral("snap")).toInt(), 7);
        apps->handle(QStringLiteral("ui_done"), {{QStringLiteral("req"), sent.at(0).value(QStringLiteral("req"))},
                                                 {QStringLiteral("ok"), true}});
        QTRY_VERIFY(tap.isFinished());
        QCOMPARE(tap.reply().type(), QDBusMessage::ReplyMessage);

        // Keel's context: the same screen, bounded.
        sent.clear();
        QDBusPendingCallWatcher context(bus(QStringLiteral("client")).asyncCall(
            QDBusMessage::createMethodCall(name, QStringLiteral("/org/shipwright/Keel/Actions"),
                                           QStringLiteral("org.shipwright.Keel.Actions"), QStringLiteral("GetContext")),
            20000));
        QTRY_COMPARE(sent.size(), 1);
        apps->handle(QStringLiteral("screen"), {{QStringLiteral("req"), sent.at(0).value(QStringLiteral("req"))},
                                                {QStringLiteral("ok"), true},
                                                {QStringLiteral("pkg"), QStringLiteral("org.example.chat")},
                                                {QStringLiteral("snap"), 8},
                                                {QStringLiteral("nodes"), QJsonArray()}});
        QTRY_VERIFY(context.isFinished());
        QCOMPARE(QJsonDocument::fromJson(json(context.reply()).toUtf8()).object().value(QStringLiteral("app")).toString(),
                 name);

        // Locked: nothing runs.
        fakeShell.locked = true;
        reply = call(name, QStringLiteral("Invoke"), {QStringLiteral("app.search"), QStringLiteral("{\"query\":\"x\"}"), QVariantMap()});
        QCOMPARE(reply.errorName(), QStringLiteral("org.shipwright.Keel.Actions.Error.NotAvailable"));
        fakeShell.locked = false;

        // Android going away fails what it was asked.
        sent.clear();
        QDBusPendingCallWatcher lost(bus(QStringLiteral("client")).asyncCall(
            [&] {
                QDBusMessage m = QDBusMessage::createMethodCall(name, QStringLiteral("/org/shipwright/Keel/Actions"),
                                                                QStringLiteral("org.shipwright.Keel.Actions"),
                                                                QStringLiteral("Invoke"));
                m << QStringLiteral("screen.read") << QStringLiteral("{}") << QVariantMap();
                return m;
            }(),
            20000));
        QTRY_COMPARE(sent.size(), 1);
        apps->setConnected(false);
        QTRY_VERIFY(lost.isFinished());
        QCOMPARE(lost.reply().errorName(), QStringLiteral("org.shipwright.Keel.Actions.Error.NotAvailable"));
        apps->setConnected(true);

        // A changed manifest comes on a new owner, so keel-mcp describes it again.
        const QString owner = daemon->serviceOwner(name).value();
        QJsonObject   changed = sample();
        changed.insert(QStringLiteral("label"), QStringLiteral("Chat 2"));
        apps->handle(QStringLiteral("app_info"), changed);
        QTRY_VERIFY(daemon->isServiceRegistered(name));
        QVERIFY(daemon->serviceOwner(name).value() != owner);
        const QString sameOwner = daemon->serviceOwner(name).value();
        apps->handle(QStringLiteral("app_info"), changed);
        QCOMPARE(daemon->serviceOwner(name).value(), sameOwner);

        // The catalogue survives a restart of deckardd.
        apps.reset();
        QTRY_VERIFY(!daemon->isServiceRegistered(name));
        apps = std::make_unique<KeelApps>([&sent](const QJsonObject &m) { sent << m; },
                                          [this](const QString &n) { return bus(n); }, bus(QStringLiteral("deckard")),
                                          QStringList{QStringLiteral("/nonexistent/keel-mcp")}, statePath);
        QVERIFY(daemon->isServiceRegistered(name));
        // Not keel-mcp nor Pilot: refused.
        reply = call(name, QStringLiteral("Invoke"), {QStringLiteral("app.show"), QStringLiteral("{}"), QVariantMap()});
        QCOMPARE(reply.errorName(), QStringLiteral("org.freedesktop.DBus.Error.AccessDenied"));
        QCOMPARE(QJsonDocument::fromJson(json(call(name, QStringLiteral("Describe"), {})).toUtf8())
                     .object()
                     .value(QStringLiteral("appId"))
                     .toString(),
                 name);

        apps->handle(QStringLiteral("app_list"), {{QStringLiteral("pkgs"), QJsonArray()}});
        QTRY_VERIFY(!daemon->isServiceRegistered(name));
        QVERIFY(apps->packages().isEmpty());
    }
};

QTEST_GUILESS_MAIN(TestKeelApps)
#include "test_keelapps.moc"
