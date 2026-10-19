#include "packages.h"

#include <QDir>
#include <QFile>
#include <QJsonArray>
#include <QJsonObject>
#include <QSignalSpy>
#include <QTemporaryDir>
#include <QTest>

// Records replies instead of putting them on a bus.
class TestablePackages : public Packages {
  public:
    using Packages::Packages;
    QList<QDBusMessage> replies;

  protected:
    void sendReply(const QDBusMessage &reply) override {
        replies.append(reply);
    }
};

class TestPackages : public QObject {
    Q_OBJECT

    static QDBusMessage call() {
        return QDBusMessage::createMethodCall(QStringLiteral("org.marathonos.Deckard"),
                                              QStringLiteral("/org/marathonos/Deckard"),
                                              QStringLiteral("org.marathonos.Deckard1.Packages"),
                                              QStringLiteral("InstallPackage"));
    }

  private Q_SLOTS:
    void installsThroughTheBridge() {
        QTemporaryDir dir;
        const QString install = dir.filePath(QStringLiteral("install"));
        QList<QJsonObject> sent;
        QObject  root;
        bool     mayChange = true;
        QList<QDBusMessage> *packagesReplies = nullptr;
        TestablePackages packages(&root, [&](const QJsonObject &m) { sent.append(m); return true; },
                          [&](const QDBusMessage &call, std::function<void()> run) {
                              if (mayChange)
                                  run();
                              else
                                  packagesReplies->append(call.createErrorReply(QDBusError::AccessDenied, QStringLiteral("no")));
                          },
                          install);
        packagesReplies = &packages.replies;
        QSignalSpy finished(&packages, &Packages::Finished);
        QSignalSpy progress(&packages, &Packages::Progress);
        QSignalSpy changed(&packages, &Packages::PackageChanged);

        const QString notApk = dir.filePath(QStringLiteral("notes.txt"));
        QFile text(notApk);
        QVERIFY(text.open(QIODevice::WriteOnly));
        text.write("hello world");
        text.close();
        packages.InstallPackage(notApk, call());
        QVERIFY(sent.isEmpty());
        QCOMPARE(packages.replies.size(), 1);
        QCOMPARE(packages.replies[0].type(), QDBusMessage::ErrorMessage);

        const QString apk = dir.filePath(QStringLiteral("app.apk"));
        QFile zip(apk);
        QVERIFY(zip.open(QIODevice::WriteOnly));
        zip.write(QByteArrayLiteral("PK\x03\x04") + QByteArray(100, 'x'));
        zip.close();

        mayChange = false;
        packages.InstallPackage(apk, call());
        QCOMPARE(packages.replies.last().errorName(), QStringLiteral("org.freedesktop.DBus.Error.AccessDenied"));
        mayChange = true;

        QVERIFY(packages.InstallPackage(apk, call()).isEmpty());
        QCOMPARE(packages.replies.last().type(), QDBusMessage::ReplyMessage);
        const QString job = packages.replies.last().arguments().constFirst().toString();
        QVERIFY(!job.isEmpty());
        QCOMPARE(sent.size(), 1);
        QCOMPARE(sent[0].value(QStringLiteral("t")).toString(), QStringLiteral("install"));
        const QString androidPath = sent[0].value(QStringLiteral("path")).toString();
        QVERIFY(androidPath.startsWith(QStringLiteral("/data/deckard/install/")));
        const QString staged = install + u'/' + androidPath.section(u'/', -1);
        QVERIFY(QFile::exists(staged));

        packages.handle(QStringLiteral("install_progress"),
                        {{QStringLiteral("id"), job}, {QStringLiteral("percent"), 250}});
        QCOMPARE(progress.size(), 1);
        QCOMPARE(progress[0][1].toInt(), 100);

        packages.handle(QStringLiteral("install_result"), {{QStringLiteral("id"), QStringLiteral("other")},
                                                           {QStringLiteral("ok"), true}});
        QCOMPARE(finished.size(), 0);
        packages.handle(QStringLiteral("install_result"), {{QStringLiteral("id"), job},
                                                           {QStringLiteral("ok"), true},
                                                           {QStringLiteral("pkg"), QStringLiteral("org.example.app")}});
        QCOMPARE(finished.size(), 1);
        QCOMPARE(finished[0][1].toBool(), true);
        QCOMPARE(finished[0][2].toString(), QStringLiteral("org.example.app"));
        QVERIFY(!QFile::exists(staged));

        packages.handle(QStringLiteral("package_changed"), {{QStringLiteral("pkg"), QStringLiteral("org.example.app")},
                                                            {QStringLiteral("change"), QStringLiteral("installed")}});
        packages.handle(QStringLiteral("package_changed"), {{QStringLiteral("pkg"), QStringLiteral("../x")},
                                                            {QStringLiteral("change"), QStringLiteral("installed")}});
        QCOMPARE(changed.size(), 1);
    }

    void knowsAnExecutable() {
        QVERIFY(Packages::runs(QCoreApplication::applicationPid(), QCoreApplication::applicationFilePath()));
        QVERIFY(!Packages::runs(QCoreApplication::applicationPid(), QStringLiteral("/bin/sh")));
        QVERIFY(!Packages::runs(-1, QCoreApplication::applicationFilePath()));
    }

    // What an .apk needs and what the phone has, checked on the way through.
    void requirementsAndDeviceProfile() {
        QTemporaryDir dir;
        QObject  root;
        QList<QJsonObject> sent;
        TestablePackages packages(&root, [&](const QJsonObject &m) { sent.append(m); return true; },
                          [](const QDBusMessage &, std::function<void()> run) { run(); },
                          dir.filePath(QStringLiteral("i")));
        const QString apk = dir.filePath(QStringLiteral("a.apk"));
        QFile f(apk);
        QVERIFY(f.open(QIODevice::WriteOnly));
        f.write("PK\x03\x04 rest");
        f.close();
        packages.InspectPackage(apk, call());
        const QString id = sent.last().value(QStringLiteral("id")).toString();
        packages.handle(QStringLiteral("inspect_result"),
                        {{QStringLiteral("id"), id}, {QStringLiteral("ok"), true},
                         {QStringLiteral("pkg"), QStringLiteral("org.example.maps")},
                         {QStringLiteral("features"), QJsonArray{QStringLiteral("android.hardware.telephony"), QStringLiteral("bad feature")}},
                         {QStringLiteral("libraries"), QJsonArray{QStringLiteral("com.google.android.maps")}},
                         {QStringLiteral("abis"), QJsonArray{QStringLiteral("armeabi-v7a"), QStringLiteral("../x")}},
                         {QStringLiteral("gms"), true}});
        const QVariantMap info = packages.replies.last().arguments().constFirst().toMap();
        QCOMPARE(info.value(QStringLiteral("features")).toStringList(), QStringList{QStringLiteral("android.hardware.telephony")});
        QCOMPARE(info.value(QStringLiteral("libraries")).toStringList(), QStringList{QStringLiteral("com.google.android.maps")});
        QCOMPARE(info.value(QStringLiteral("abis")).toStringList(), QStringList{QStringLiteral("armeabi-v7a")});
        QVERIFY(info.value(QStringLiteral("gms")).toBool());

        packages.DeviceProfile(call());
        QCOMPARE(sent.last().value(QStringLiteral("t")).toString(), QStringLiteral("device"));
        packages.handle(QStringLiteral("device_result"),
                        {{QStringLiteral("id"), sent.last().value(QStringLiteral("id"))},
                         {QStringLiteral("features"), QJsonArray{QStringLiteral("android.hardware.camera.any")}},
                         {QStringLiteral("abis"), QJsonArray{QStringLiteral("arm64-v8a")}},
                         {QStringLiteral("sdk"), 33}});
        const QVariantMap device = packages.replies.last().arguments().constFirst().toMap();
        QCOMPARE(device.value(QStringLiteral("abis")).toStringList(), QStringList{QStringLiteral("arm64-v8a")});
        QCOMPARE(device.value(QStringLiteral("sdk")).toInt(), 33);
        QVERIFY(!device.value(QStringLiteral("gms")).toBool());
    }

    void androidGoneFailsJobs() {
        QTemporaryDir dir;
        QObject  root;
        TestablePackages packages(&root, [](const QJsonObject &) { return true; },
                          [](const QDBusMessage &, std::function<void()> run) { run(); },
                          dir.filePath(QStringLiteral("i")));
        QSignalSpy finished(&packages, &Packages::Finished);
        packages.UninstallPackage(QStringLiteral("org.example.app"), call());
        const QString job = packages.replies.last().arguments().constFirst().toString();
        QVERIFY(!job.isEmpty());
        packages.UninstallPackage(QStringLiteral("bad name"), call());
        QCOMPARE(packages.replies.last().type(), QDBusMessage::ErrorMessage);
        packages.bridgeLost();
        QCOMPARE(finished.size(), 1);
        QCOMPARE(finished[0][0].toString(), job);
        QCOMPARE(finished[0][1].toBool(), false);
    }
};

QTEST_GUILESS_MAIN(TestPackages)
#include "test_packages.moc"
