#include "reclaimer.h"

#include <QFile>
#include <QJsonArray>
#include <QSignalSpy>
#include <QTemporaryDir>
#include <QTest>

class TestReclaimer : public QObject {
    Q_OBJECT

    static void writeEvents(const QString &dir, int high, int max) {
        QFile f(dir + QStringLiteral("/memory.events"));
        QVERIFY(f.open(QIODevice::WriteOnly | QIODevice::Truncate));
        f.write(QStringLiteral("low 0\nhigh %1\nmax %2\noom 0\noom_kill 0\noom_group_kill 0\n")
                    .arg(high)
                    .arg(max)
                    .toLatin1());
    }

  private Q_SLOTS:
    void countsHighAndMax() {
        QCOMPARE(Reclaimer::overBudget("low 4\nhigh 74\nmax 3\noom 1\n"), quint64(77));
        QCOMPARE(Reclaimer::overBudget(""), quint64(0));
    }

    void asksWhenTheContainerGoesOverBudget() {
        QTemporaryDir dir;
        writeEvents(dir.path(), 5, 0);
        QList<QJsonObject> sent;
        Reclaimer r({dir.path(), dir.path() + QStringLiteral("/none")},
                    [&](const QJsonObject &m) { sent.append(m); });
        QSignalSpy requested(&r, &Reclaimer::requested);

        writeEvents(dir.path(), 6, 0);
        QTest::qWait(200);
        QCOMPARE(sent.size(), 0);

        r.setConnected(true);
        writeEvents(dir.path(), 6, 0);
        QTest::qWait(200);
        QCOMPARE(sent.size(), 0);

        writeEvents(dir.path(), 9, 0);
        QTRY_COMPARE(sent.size(), 1);
        QCOMPARE(sent[0].value(QStringLiteral("t")).toString(), QStringLiteral("reclaim"));
        QCOMPARE(sent[0].value(QStringLiteral("count")).toInt(), Reclaimer::kCount);
        QCOMPARE(sent[0].value(QStringLiteral("reason")).toString(), QStringLiteral("container-high"));

        // At most one request per gap, however often the counter moves.
        writeEvents(dir.path(), 12, 1);
        QTest::qWait(200);
        QCOMPARE(sent.size(), 1);
        QTest::qWait(Reclaimer::kGapMs);
        writeEvents(dir.path(), 13, 1);
        QTRY_COMPARE(sent.size(), 2);

        // Nothing left to end: back off.
        r.handle(QStringLiteral("reclaimed"), {{QStringLiteral("ok"), true}, {QStringLiteral("killed"), QJsonArray()}});
        QTest::qWait(Reclaimer::kGapMs + 200);
        writeEvents(dir.path(), 14, 1);
        QTest::qWait(300);
        QCOMPARE(sent.size(), 2);

        r.setConnected(false);
        QTest::qWait(Reclaimer::kIdleGapMs);
        writeEvents(dir.path(), 20, 1);
        QTest::qWait(300);
        QCOMPARE(sent.size(), 2);
        QCOMPARE(requested.size(), 2);
    }
};

QTEST_GUILESS_MAIN(TestReclaimer)
#include "test_reclaimer.moc"
