#pragma once

#include <QColor>
#include <QList>
#include <QObject>
#include <QPair>
#include <QString>

// Android's 65 system_accent/neutral colours from Marathon's tokens and the
// ambience highlight, as image/vendor_deckard/overlay/DeckardPalette's
// gen-palette.py makes them for the boot default.
namespace Palette {
    QList<QPair<QString, QColor>> generate(const QColor &highlight);
}

// Writes the palette for the current ambience to /data/deckard/palette,
// where Android's deckard-palette service applies it as a fabricated overlay.
class AmbiencePalette : public QObject {
    Q_OBJECT

  public:
    AmbiencePalette(const QString &path, QObject *parent = nullptr);

  private Q_SLOTS:
    void refresh();
    void onNameOwnerChanged(const QString &name);

  private:
    void apply(const QVariantMap &ambience);

    QString m_path;
};
