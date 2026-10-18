#include "palette.h"

#include <QFile>
#include <QRegularExpression>
#include <QTest>

class TestPalette : public QObject {
    Q_OBJECT

  private Q_SLOTS:
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
