#include "bootwatchdog.h"

#include <QDateTime>
#include <QFile>
#include <QSignalSpy>
#include <QTemporaryDir>
#include <QTest>

class TestBootWatchdog : public QObject {
    Q_OBJECT

    static void touch(const QString &path) {
        QFile f(path);
        QVERIFY(f.open(QIODevice::WriteOnly | QIODevice::Truncate));
        f.write("1");
    }

    // Written by an earlier boot.
    static void age(const QString &path) {
        QFile f(path);
        QVERIFY(f.open(QIODevice::ReadWrite));
        QVERIFY(f.setFileTime(QDateTime::currentDateTime().addSecs(-600), QFileDevice::FileModificationTime));
    }

  private Q_SLOTS:
    void leavesABootThatCompleted() {
        QTemporaryDir dir;
        const QString marker = dir.path() + QStringLiteral("/booted");
        int           restarts = 0;
        BootWatchdog  w({marker, {}}, [&] { ++restarts; });
        const qint64  now = QDateTime::currentMSecsSinceEpoch();
        w.setActiveSince(now - BootWatchdog::kBootMs + 300);
        QTest::qWait(50);
        touch(marker);
        QTest::qWait(600);
        QCOMPARE(restarts, 0);
    }

    void restartsAStalledBootOnce() {
        QTemporaryDir dir;
        const QString marker = dir.path() + QStringLiteral("/booted");
        touch(marker);
        age(marker);
        int          restarts = 0;
        BootWatchdog w({marker, {QStringLiteral("sh"), QStringLiteral("-c"), QStringLiteral("echo stuck")}},
                       [&] { ++restarts; });
        QSignalSpy   stalled(&w, &BootWatchdog::stalled);
        qint64       start = QDateTime::currentMSecsSinceEpoch() + 1000;
        w.setActiveSince(start - BootWatchdog::kBootMs);
        QTRY_COMPARE(restarts, 1);
        QCOMPARE(stalled.size(), 1);

        // The restarted session stalls as well: reported, not restarted.
        w.setActiveSince(0);
        w.setActiveSince(start - BootWatchdog::kBootMs + 1);
        QTRY_COMPARE(stalled.size(), 2);
        QTest::qWait(300);
        QCOMPARE(restarts, 1);

        // A completed boot re-arms the restart.
        w.setActiveSince(0);
        start = QDateTime::currentMSecsSinceEpoch() - 10;
        touch(marker);
        w.setActiveSince(start - BootWatchdog::kBootMs);
        QTest::qWait(300);
        QCOMPARE(restarts, 1);
        w.setActiveSince(0);
        age(marker);
        w.setActiveSince(QDateTime::currentMSecsSinceEpoch() + 1000 - BootWatchdog::kBootMs);
        QTRY_COMPARE(restarts, 2);
    }

    void noRestartOnceTheSessionStopped() {
        QTemporaryDir dir;
        int           restarts = 0;
        BootWatchdog  w({dir.path() + QStringLiteral("/booted"),
                        {QStringLiteral("sh"), QStringLiteral("-c"), QStringLiteral("sleep 0.5")}},
                       [&] { ++restarts; });
        QSignalSpy    stalled(&w, &BootWatchdog::stalled);
        w.setActiveSince(QDateTime::currentMSecsSinceEpoch() - BootWatchdog::kBootMs);
        QTRY_COMPARE(stalled.size(), 1);
        w.setActiveSince(0);
        QTest::qWait(1000);
        QCOMPARE(restarts, 0);
    }
};

QTEST_GUILESS_MAIN(TestBootWatchdog)
#include "test_bootwatchdog.moc"
