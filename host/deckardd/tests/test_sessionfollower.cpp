#include "sessionfollower.h"

#include <QTest>

class TestSessionFollower : public QObject {
    Q_OBJECT

    static const QString kShell;

  private Q_SLOTS:
    void restartsOnlyAfterTheShellCameBack() {
        int             restarts = 0;
        SessionFollower follower([&] { ++restarts; });
        follower.onNameOwnerChanged(kShell, QString(), QStringLiteral(":1.5"));
        follower.onNameOwnerChanged(QStringLiteral("org.other"), QStringLiteral(":1.6"), QString());
        follower.onNameOwnerChanged(QStringLiteral("org.other"), QString(), QStringLiteral(":1.7"));
        QTest::qWait(SessionFollower::kSettleMs + 300);
        QCOMPARE(restarts, 0);

        follower.onNameOwnerChanged(kShell, QStringLiteral(":1.5"), QString());
        follower.onNameOwnerChanged(kShell, QString(), QStringLiteral(":1.9"));
        QCOMPARE(restarts, 0);
        QTRY_COMPARE_WITH_TIMEOUT(restarts, 1, SessionFollower::kSettleMs + 1000);

        // A crash loop restarts Android once, not on every shell start.
        follower.onNameOwnerChanged(kShell, QStringLiteral(":1.9"), QString());
        follower.onNameOwnerChanged(kShell, QString(), QStringLiteral(":1.12"));
        QTest::qWait(SessionFollower::kSettleMs + 300);
        QCOMPARE(restarts, 1);
    }
};

const QString TestSessionFollower::kShell = QStringLiteral("org.marathonos.Shell");

QTEST_GUILESS_MAIN(TestSessionFollower)
#include "test_sessionfollower.moc"
