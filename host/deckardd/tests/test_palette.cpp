#include "palette.h"

#include <QDBusConnection>
#include <QDBusMessage>
#include <QFile>
#include <QProcess>
#include <QRegularExpression>
#include <QStandardPaths>
#include <QTemporaryDir>
#include <QTest>

// Marathon's Ambience1, enough for AmbiencePalette.
class FakeAmbience : public QObject {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.marathonos.Shell.Ambience1")

  public:
    QString highlight = QStringLiteral("#ff7df0d4");

    void change(const QString &color) {
        highlight = color;
        QDBusMessage signal = QDBusMessage::createSignal(QStringLiteral("/org/marathonos/Shell/Ambience"),
                                                         QStringLiteral("org.marathonos.Shell.Ambience1"),
                                                         QStringLiteral("AmbienceChanged"));
        signal << GetAmbience();
        QDBusConnection::sessionBus().send(signal);
    }

  public Q_SLOTS:
    QVariantMap GetAmbience() {
        return {{QStringLiteral("/desktop/jolla/theme/color/highlight"), highlight}};
    }
};

class TestPalette : public QObject {
    Q_OBJECT

    QProcess m_bus;

    static QHash<QString, QString> read(const QString &path) {
        QHash<QString, QString> colors;
        QFile file(path);
        if (file.open(QIODevice::ReadOnly)) {
            for (const QByteArray &line : file.readAll().split('\n')) {
                const QList<QByteArray> f = line.split(' ');
                if (f.size() == 2)
                    colors.insert(QString::fromLatin1(f[0]), QString::fromLatin1(f[1]));
            }
        }
        return colors;
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

    // The ambience reaches the palette file, and a new highlight rewrites it.
    void followsAmbience1() {
        if (m_bus.state() != QProcess::Running)
            QSKIP("no dbus-daemon");
        FakeAmbience shell;
        QDBusConnection bus = QDBusConnection::sessionBus();
        QVERIFY(bus.registerObject(QStringLiteral("/org/marathonos/Shell/Ambience"), &shell,
                                   QDBusConnection::ExportAllSlots));
        QVERIFY(bus.registerService(QStringLiteral("org.marathonos.Shell")));
        QTemporaryDir dir;
        const QString path = dir.filePath(QStringLiteral("deckard/palette"));
        AmbiencePalette palette(path);
        QTRY_COMPARE(read(path).value(QStringLiteral("system_accent1_100")), QStringLiteral("#ff7df0d4"));
        QCOMPARE(read(path).size(), 65);
        shell.change(QStringLiteral("#ffff8a3d"));
        QTRY_COMPARE(read(path).value(QStringLiteral("system_accent1_100")), QStringLiteral("#ffff8a3d"));
        QCOMPARE(read(path).value(QStringLiteral("system_neutral1_900")), QStringLiteral("#ff040404"));
    }

    // The boot default (DeckardPalette, from gen-palette.py) and the runtime
    // palette must agree for the default ambience.
    void matchesGenPalette() {
        QFile xml(QStringLiteral(DECKARD_PALETTE_XML));
        QVERIFY(xml.open(QIODevice::ReadOnly));
        QHash<QString, QString> expected;
        static const QRegularExpression kColor(
            QStringLiteral("<color name=\"(\\w+)\">(#[0-9a-f]{6})</color>"));
        auto it = kColor.globalMatch(QString::fromUtf8(xml.readAll()));
        while (it.hasNext()) {
            const auto m = it.next();
            expected.insert(m.captured(1), m.captured(2));
        }
        QCOMPARE(expected.size(), 65);
        const auto palette = Palette::generate(QColor(0x7df0d4));
        QCOMPARE(palette.size(), 65);
        for (const auto &[name, color] : palette)
            QCOMPARE(color.name(), expected.value(name));
    }

    void followsTheHighlight() {
        const auto palette = Palette::generate(QColor(QStringLiteral("#ffff6b9d")));
        QHash<QString, QColor> byName;
        for (const auto &[name, color] : palette)
            byName.insert(name, color);
        QCOMPARE(byName.value(QStringLiteral("system_accent1_100")), QColor(0xff6b9d));
        QCOMPARE(byName.value(QStringLiteral("system_accent3_200")), QColor(0xff6b9d));
        QCOMPARE(byName.value(QStringLiteral("system_neutral1_900")), QColor(0x040404));
        const QColor mid = byName.value(QStringLiteral("system_accent1_500"));
        QVERIFY(mid.red() > mid.green());
    }
};

QTEST_GUILESS_MAIN(TestPalette)
#include "test_palette.moc"
