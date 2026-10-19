#include "packages.h"

#include <QDir>
#include <QFile>
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
        TestablePackages packages(&root, [&](const QJsonObject &m) { sent.append(m); return true; },
                          [&](const QDBusMessage &) { return mayChange; }, install);
        QSignalSpy finished(&packages, &Packages::Finished);
        QSignalSpy progress(&packages, &Packages::Progress);
        QSignalSpy changed(&packages, &Packages::PackageChanged);

        const QString notApk = dir.filePath(QStringLiteral("notes.txt"));
        QFile text(notApk);
        QVERIFY(text.open(QIODevice::WriteOnly));
        text.write("hello world");
        text.close();
        QVERIFY(packages.InstallPackage(notApk, call()).isEmpty());
        QVERIFY(sent.isEmpty());
        QCOMPARE(packages.replies.size(), 1);
        QCOMPARE(packages.replies[0].type(), QDBusMessage::ErrorMessage);

        const QString apk = dir.filePath(QStringLiteral("app.apk"));
        QFile zip(apk);
        QVERIFY(zip.open(QIODevice::WriteOnly));
        zip.write(QByteArrayLiteral("PK\x03\x04") + QByteArray(100, 'x'));
        zip.close();

        mayChange = false;
        QVERIFY(packages.InstallPackage(apk, call()).isEmpty());
        QCOMPARE(packages.replies.last().errorName(), QStringLiteral("org.freedesktop.DBus.Error.AccessDenied"));
        mayChange = true;

        const QString job = packages.InstallPackage(apk, call());
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

    void androidGoneFailsJobs() {
        QTemporaryDir dir;
        QObject  root;
        TestablePackages packages(&root, [](const QJsonObject &) { return true; },
                          [](const QDBusMessage &) { return true; }, dir.filePath(QStringLiteral("i")));
        QSignalSpy finished(&packages, &Packages::Finished);
        const QString job = packages.UninstallPackage(QStringLiteral("org.example.app"), call());
        QVERIFY(!job.isEmpty());
        QVERIFY(packages.UninstallPackage(QStringLiteral("bad name"), call()).isEmpty());
        packages.bridgeLost();
        QCOMPARE(finished.size(), 1);
        QCOMPARE(finished[0][0].toString(), job);
        QCOMPARE(finished[0][1].toBool(), false);
    }
};

QTEST_GUILESS_MAIN(TestPackages)
#include "test_packages.moc"
