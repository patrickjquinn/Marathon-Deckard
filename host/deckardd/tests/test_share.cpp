#include "share.h"

#include <QBuffer>
#include <QDBusConnection>
#include <QDBusUnixFileDescriptor>
#include <QDir>
#include <QFile>
#include <QImage>
#include <QJsonArray>
#include <QProcess>
#include <QStandardPaths>
#include <QTemporaryDir>
#include <QTest>

#include <fcntl.h>
#include <unistd.h>

// Records replies instead of putting them on a bus.
class TestableSharing : public Sharing {
  public:
    using Sharing::Sharing;
    QList<QDBusMessage> replies;

  protected:
    void sendReply(const QDBusMessage &reply) override {
        replies.append(reply);
    }
};

// The shell's org.sailfishos.share, recording what it was asked.
class FakeShareSheet : public QObject {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.sailfishos.share")

  public:
    QList<QVariantMap> requests;

  public Q_SLOTS:
    Q_SCRIPTABLE void share(const QVariantMap &configuration) {
        requests.append(Sharing::plain(configuration).toMap());
    }
};

class TestShare : public QObject {
    Q_OBJECT

    QProcess m_bus;

    static QDBusMessage call(const QString &method) {
        return QDBusMessage::createMethodCall(QStringLiteral("org.marathonos.Deckard"),
                                              QStringLiteral("/org/marathonos/Deckard"),
                                              QStringLiteral("org.marathonos.Deckard1.Share"), method);
    }

    static QString pngBase64() {
        QImage image(200, 100, QImage::Format_ARGB32);
        image.fill(Qt::red);
        QByteArray png;
        QBuffer    buffer(&png);
        buffer.open(QIODevice::WriteOnly);
        image.save(&buffer, "PNG");
        return QString::fromLatin1(png.toBase64());
    }

    static QDBusUnixFileDescriptor fdFor(const QString &path) {
        const int fd = ::open(QFile::encodeName(path).constData(), O_RDONLY | O_CLOEXEC);
        QDBusUnixFileDescriptor d(fd);
        ::close(fd);
        return d;
    }

    static QByteArray readFd(const QDBusUnixFileDescriptor &d) {
        QFile f;
        if (!f.open(d.fileDescriptor(), QIODevice::ReadOnly, QFileDevice::DontCloseHandle))
            return {};
        return f.readAll();
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

    void targetIds() {
        QVERIFY(Sharing::isTargetId(QStringLiteral("org.example.chat/org.example.chat.ShareActivity")));
        QVERIFY(Sharing::isTargetId(QStringLiteral("org.example.chat/.Share$Inner")));
        QVERIFY(!Sharing::isTargetId(QStringLiteral("org.example.chat")));
        QVERIFY(!Sharing::isTargetId(QStringLiteral("org.example.chat/a/b")));
        QVERIFY(!Sharing::isTargetId(QStringLiteral("../x/Activity")));
        QVERIFY(!Sharing::isTargetId(QStringLiteral("org.example.chat/") + QString(260, u'a')));
    }

    void targetsOnlyForTheShell() {
        QTemporaryDir      dir;
        QObject            root;
        QList<QJsonObject> sent;
        bool               shell = false;
        bool               connected = true;
        TestableSharing    sharing(&root, QDBusConnection::sessionBus(),
                                   [&](const QJsonObject &m) { sent.append(m); return connected; },
                                   [&](const QDBusMessage &) { return shell; }, dir.path());

        sharing.ShareTargets(QStringLiteral("image/png"), call(QStringLiteral("ShareTargets")));
        QCOMPARE(sharing.replies.size(), 1);
        QCOMPARE(sharing.replies[0].errorName(), QStringLiteral("org.freedesktop.DBus.Error.AccessDenied"));
        QVERIFY(sent.isEmpty());

        shell     = true;
        connected = false;
        sharing.ShareTargets(QStringLiteral("image/png"), call(QStringLiteral("ShareTargets")));
        QCOMPARE(sharing.replies.size(), 2);
        QCOMPARE(sharing.replies[1].type(), QDBusMessage::ReplyMessage);
        QVERIFY(sharing.replies[1].arguments().first().value<ShareTargetList>().isEmpty());

        connected = true;
        sharing.ShareTargets(QStringLiteral("image/png"), call(QStringLiteral("ShareTargets")));
        QCOMPARE(sent.last().value(QStringLiteral("t")).toString(), QStringLiteral("share_targets"));
        QCOMPARE(sent.last().value(QStringLiteral("mime")).toString(), QStringLiteral("image/png"));
        const QString id = sent.last().value(QStringLiteral("id")).toString();
        const QJsonArray targets{
            QJsonObject{{QStringLiteral("pkg"), QStringLiteral("org.example.chat")},
                        {QStringLiteral("activity"), QStringLiteral("org.example.chat.ShareActivity")},
                        {QStringLiteral("label"), QStringLiteral("Chat")},
                        {QStringLiteral("icon"), pngBase64()},
                        {QStringLiteral("multiple"), true}},
            QJsonObject{{QStringLiteral("pkg"), QStringLiteral("bad pkg")},
                        {QStringLiteral("activity"), QStringLiteral("A")}},
            QJsonObject{{QStringLiteral("pkg"), QStringLiteral("org.marathonos.deckard.system")},
                        {QStringLiteral("activity"), QStringLiteral("org.marathonos.deckard.system.Chooser")}},
            QJsonObject{{QStringLiteral("pkg"), QStringLiteral("org.example.notes")},
                        {QStringLiteral("activity"), QStringLiteral("org.example.notes/Evil")}},
            QJsonObject{{QStringLiteral("pkg"), QStringLiteral("org.example.notes")},
                        {QStringLiteral("activity"), QStringLiteral("org.example.notes.Add")},
                        {QStringLiteral("label"), QStringLiteral("Add to Waydroid notes")},
                        {QStringLiteral("icon"), QStringLiteral("not base64 png")}}};
        sharing.handle(QStringLiteral("share_targets_result"),
                       {{QStringLiteral("id"), id}, {QStringLiteral("targets"), targets}});
        QCOMPARE(sharing.replies.size(), 3);
        const ShareTargetList list = sharing.replies[2].arguments().first().value<ShareTargetList>();
        QCOMPARE(list.size(), 2);
        QCOMPARE(list[0].value(QStringLiteral("id")).toString(),
                 QStringLiteral("org.example.chat/org.example.chat.ShareActivity"));
        QCOMPARE(list[0].value(QStringLiteral("pkg")).toString(), QStringLiteral("org.example.chat"));
        QCOMPARE(list[0].value(QStringLiteral("label")).toString(), QStringLiteral("Chat"));
        QVERIFY(list[0].value(QStringLiteral("icon")).toString().startsWith(QStringLiteral("data:image/png;base64,")));
        QVERIFY(list[0].value(QStringLiteral("multiple")).toBool());
        QCOMPARE(list[1].value(QStringLiteral("label")).toString(), QStringLiteral("Add to Android notes"));
        QCOMPARE(list[1].value(QStringLiteral("icon")).toString(), QString());
        QVERIFY(!list[1].value(QStringLiteral("multiple")).toBool());
    }

    void sharesFilesAndText() {
        QTemporaryDir      dir;
        const QString      xfer = dir.filePath(QStringLiteral("xfer"));
        QObject            root;
        QList<QJsonObject> sent;
        TestableSharing    sharing(&root, QDBusConnection::sessionBus(),
                                   [&](const QJsonObject &m) { sent.append(m); return true; },
                                   [](const QDBusMessage &) { return true; }, xfer);
        const QString photo = dir.filePath(QStringLiteral("photo.jpg"));
        QFile         f(photo);
        QVERIFY(f.open(QIODevice::WriteOnly));
        f.write("jpeg bytes");
        f.close();

        const QString target = QStringLiteral("org.example.chat/org.example.chat.ShareActivity");
        sharing.Share(QStringLiteral("not a target"), {}, call(QStringLiteral("Share")));
        QCOMPARE(sharing.replies.last().errorName(), QStringLiteral("org.freedesktop.DBus.Error.InvalidArgs"));
        sharing.Share(target, {{QStringLiteral("resources"), QVariantList()}}, call(QStringLiteral("Share")));
        QCOMPARE(sharing.replies.last().errorName(), QStringLiteral("org.freedesktop.DBus.Error.InvalidArgs"));
        int pipes[2];
        QCOMPARE(::pipe(pipes), 0);
        QDBusUnixFileDescriptor pipeEnd(pipes[0]);
        ::close(pipes[0]);
        ::close(pipes[1]);
        sharing.Share(target,
                      {{QStringLiteral("resources"),
                        QVariantList{QVariantMap{{QStringLiteral("name"), QStringLiteral("p")},
                                                 {QStringLiteral("fileDescriptor"), QVariant::fromValue(pipeEnd)}}}}},
                      call(QStringLiteral("Share")));
        QCOMPARE(sharing.replies.last().errorName(), QStringLiteral("org.freedesktop.DBus.Error.InvalidArgs"));
        const int refused = sharing.replies.size();
        QVERIFY(sent.isEmpty());

        const QVariantMap configuration{
            {QStringLiteral("mimeType"), QStringLiteral("image/jpeg")},
            {QStringLiteral("title"), QStringLiteral("Holiday")},
            {QStringLiteral("resources"),
             QVariantList{QVariantMap{{QStringLiteral("name"), QStringLiteral("../../photo.jpg")},
                                      {QStringLiteral("fileDescriptor"), QVariant::fromValue(fdFor(photo))}},
                          QVariantMap{{QStringLiteral("name"), QStringLiteral("photo.jpg")},
                                      {QStringLiteral("fileDescriptor"), QVariant::fromValue(fdFor(photo))}},
                          QVariantMap{{QStringLiteral("name"), QStringLiteral("note")},
                                      {QStringLiteral("data"), QStringLiteral("Look at this")}}}}};
        sharing.Share(target, configuration, call(QStringLiteral("Share")));
        QTRY_COMPARE(sent.size(), 1);
        const QJsonObject m = sent[0];
        QCOMPARE(m.value(QStringLiteral("t")).toString(), QStringLiteral("share"));
        QCOMPARE(m.value(QStringLiteral("target")).toString(), target);
        QCOMPARE(m.value(QStringLiteral("mime")).toString(), QStringLiteral("image/jpeg"));
        QCOMPARE(m.value(QStringLiteral("title")).toString(), QStringLiteral("Holiday"));
        QCOMPARE(m.value(QStringLiteral("text")).toString(), QStringLiteral("Look at this"));
        const QString     tid   = m.value(QStringLiteral("id")).toString();
        const QJsonArray  files = m.value(QStringLiteral("files")).toArray();
        QCOMPARE(files.size(), 2);
        QCOMPARE(files[0].toString(), QStringLiteral("photo.jpg"));
        QVERIFY(files[1].toString() != files[0].toString());
        for (const QJsonValue &name : files) {
            QFile copy(xfer + u'/' + tid + u'/' + name.toString());
            QVERIFY(copy.open(QIODevice::ReadOnly));
            QCOMPARE(copy.readAll(), QByteArrayLiteral("jpeg bytes"));
        }
        QCOMPARE(sharing.replies.size(), refused);

        sharing.handle(QStringLiteral("shared"), {{QStringLiteral("id"), tid}, {QStringLiteral("ok"), true}});
        QCOMPARE(sharing.replies.size(), refused + 1);
        QCOMPARE(sharing.replies.last().type(), QDBusMessage::ReplyMessage);

        sharing.Share(target,
                      {{QStringLiteral("resources"),
                        QVariantList{QVariantMap{{QStringLiteral("data"), QStringLiteral("hi")}}}}},
                      call(QStringLiteral("Share")));
        QTRY_COMPARE(sent.size(), 2);
        QCOMPARE(sent[1].value(QStringLiteral("mime")).toString(), QStringLiteral("text/plain"));
        sharing.handle(QStringLiteral("shared"), {{QStringLiteral("id"), sent[1].value(QStringLiteral("id"))},
                                                  {QStringLiteral("ok"), false},
                                                  {QStringLiteral("error"), QStringLiteral("no activity")}});
        QCOMPARE(sharing.replies.last().errorName(), QStringLiteral("org.marathonos.Deckard1.Error.Failed"));

        sharing.Share(target,
                      {{QStringLiteral("resources"),
                        QVariantList{QVariantMap{{QStringLiteral("data"), QStringLiteral("hi")}}}}},
                      call(QStringLiteral("Share")));
        QTRY_COMPARE(sent.size(), 3);
        sharing.systemLost();
        QCOMPARE(sharing.replies.last().errorName(), QStringLiteral("org.marathonos.Deckard1.Error.Failed"));
    }

    void androidSharesOpenTheSheet() {
        if (m_bus.state() != QProcess::Running)
            QSKIP("no dbus-daemon");
        FakeShareSheet  sheet;
        QDBusConnection bus = QDBusConnection::sessionBus();
        QVERIFY(bus.registerObject(QStringLiteral("/"), &sheet, QDBusConnection::ExportScriptableSlots));
        QVERIFY(bus.registerService(QStringLiteral("org.sailfishos.share")));

        QTemporaryDir   dir;
        const QString   xfer = dir.filePath(QStringLiteral("xfer"));
        const QString   tid  = QStringLiteral("0123456789abcdef");
        QVERIFY(QDir().mkpath(xfer + u'/' + tid));
        QFile f(xfer + u'/' + tid + QStringLiteral("/scan.pdf"));
        QVERIFY(f.open(QIODevice::WriteOnly));
        f.write("%PDF");
        f.close();
        QVERIFY(QFile::link(dir.filePath(QStringLiteral("secret")), xfer + u'/' + tid + QStringLiteral("/link")));
        QObject         root;
        TestableSharing sharing(&root, bus, [](const QJsonObject &) { return true; },
                                [](const QDBusMessage &) { return true; }, xfer);

        sharing.handle(QStringLiteral("share_out"),
                       {{QStringLiteral("id"), QStringLiteral("../etc")},
                        {QStringLiteral("pkg"), QStringLiteral("org.example.scanner")},
                        {QStringLiteral("files"), QJsonArray{QStringLiteral("scan.pdf")}}});
        sharing.handle(QStringLiteral("share_out"),
                       {{QStringLiteral("id"), tid},
                        {QStringLiteral("pkg"), QStringLiteral("org.example.scanner")},
                        {QStringLiteral("files"), QJsonArray{QStringLiteral("link")}}});
        QTest::qWait(300);
        QVERIFY(sheet.requests.isEmpty());

        sharing.handle(QStringLiteral("share_out"),
                       {{QStringLiteral("id"), tid},
                        {QStringLiteral("pkg"), QStringLiteral("org.example.scanner")},
                        {QStringLiteral("mime"), QStringLiteral("application/pdf")},
                        {QStringLiteral("title"), QStringLiteral("Receipt")},
                        {QStringLiteral("text"), QStringLiteral("From the scanner")},
                        {QStringLiteral("files"), QJsonArray{QStringLiteral("scan.pdf"), QStringLiteral("../scan.pdf")}}});
        QTRY_COMPARE(sheet.requests.size(), 1);
        const QVariantMap request = sheet.requests[0];
        QCOMPARE(request.value(QStringLiteral("source")).toString(), QStringLiteral("waydroid.org.example.scanner"));
        QCOMPARE(request.value(QStringLiteral("mimeType")).toString(), QStringLiteral("application/pdf"));
        QCOMPARE(request.value(QStringLiteral("title")).toString(), QStringLiteral("Receipt"));
        const QVariantList resources = request.value(QStringLiteral("resources")).toList();
        QCOMPARE(resources.size(), 2);
        const QVariantMap file = resources[0].toMap();
        QCOMPARE(file.value(QStringLiteral("name")).toString(), QStringLiteral("scan.pdf"));
        QCOMPARE(readFd(file.value(QStringLiteral("fileDescriptor")).value<QDBusUnixFileDescriptor>()),
                 QByteArrayLiteral("%PDF"));
        QCOMPARE(resources[1].toMap().value(QStringLiteral("data")).toString(), QStringLiteral("From the scanner"));
        bus.unregisterService(QStringLiteral("org.sailfishos.share"));
        bus.unregisterObject(QStringLiteral("/"));
    }
};

QTEST_GUILESS_MAIN(TestShare)
#include "test_share.moc"
