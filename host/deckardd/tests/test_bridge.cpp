#include "bridge.h"

#include <QFile>
#include <QJsonDocument>
#include <QLocalSocket>
#include <QSignalSpy>
#include <QTemporaryDir>
#include <QTest>
#include <QtEndian>

#include <unistd.h>

namespace {
    QByteArray frame(const QJsonObject &o) {
        const QByteArray body = QJsonDocument(o).toJson(QJsonDocument::Compact);
        char             header[4];
        qToBigEndian<quint32>(quint32(body.size()), header);
        return QByteArray(header, 4) + body;
    }
} // namespace

class TestBridge : public QObject {
    Q_OBJECT

    QTemporaryDir m_dir;

    QString packages(uint uid) {
        const QString path = m_dir.filePath(QStringLiteral("packages.list"));
        QFile         file(path);
        if (!file.open(QIODevice::WriteOnly | QIODevice::Truncate))
            qFatal("cannot write %s", qPrintable(path));
        file.write("com.example.other 10001 0 /data/user/0/x default 3003\n");
        file.write(QByteArray("org.marathonos.deckard.bridge ") + QByteArray::number(uid) +
                   " 0 /data/user/0/org.marathonos.deckard.bridge platform:privapp 3003\n");
        return path;
    }

  private Q_SLOTS:
    void framesAndLimits() {
        // The test runs as an ordinary user: map the bridge to our own uid.
        const QString path = m_dir.filePath(QStringLiteral("s/bridge.sock"));
        Bridge        bridge(path, packages(getuid()));
        QVERIFY(bridge.listen());
        QSignalSpy connected(&bridge, &Bridge::connectedChanged);
        QSignalSpy received(&bridge, &Bridge::received);

        QLocalSocket client;
        client.connectToServer(path);
        QVERIFY(client.waitForConnected(1000));
        QTRY_COMPARE(connected.count(), 1);
        QVERIFY(bridge.isConnected());

        // Split across writes: frames are reassembled.
        const QByteArray hello = frame({{QStringLiteral("t"), QStringLiteral("hello")}});
        client.write(hello.left(3));
        client.flush();
        QTest::qWait(20);
        client.write(hello.mid(3));
        QTRY_COMPARE(received.count(), 1);
        QCOMPARE(received.first().at(0).toString(), QStringLiteral("hello"));

        QByteArray burst;
        for (int i = 0; i < 300; ++i)
            burst += frame({{QStringLiteral("t"), QStringLiteral("x")}});
        client.write(burst);
        QTRY_VERIFY(received.count() >= Bridge::kBurst - 1);
        QTest::qWait(100);
        QVERIFY2(received.count() < 1 + Bridge::kBurst + 20, qPrintable(QString::number(received.count())));

        const QByteArray huge = QByteArray::fromHex("7fffffff");
        client.write(huge);
        QTRY_COMPARE(client.state(), QLocalSocket::UnconnectedState);
        QTRY_COMPARE(connected.count(), 2);
        QVERIFY(!bridge.isConnected());
    }

    void malformedDrops() {
        const QString path = m_dir.filePath(QStringLiteral("m/bridge.sock"));
        Bridge        bridge(path, packages(getuid()));
        QVERIFY(bridge.listen());
        QLocalSocket client;
        client.connectToServer(path);
        QVERIFY(client.waitForConnected(1000));
        const QByteArray body = "[1,2]";
        char             header[4];
        qToBigEndian<quint32>(quint32(body.size()), header);
        client.write(QByteArray(header, 4) + body);
        QTRY_COMPARE(client.state(), QLocalSocket::UnconnectedState);
    }

    void refusesOtherUids() {
        Bridge bridge(m_dir.filePath(QStringLiteral("r/bridge.sock")), packages(getuid() + 1));
        QVERIFY(bridge.listen());
        QSignalSpy   connected(&bridge, &Bridge::connectedChanged);
        QLocalSocket client;
        client.connectToServer(m_dir.filePath(QStringLiteral("r/bridge.sock")));
        QVERIFY(client.waitForConnected(1000));
        QTRY_COMPARE(client.state(), QLocalSocket::UnconnectedState);
        QCOMPARE(connected.count(), 0);
    }
};

QTEST_GUILESS_MAIN(TestBridge)
#include "test_bridge.moc"
