#pragma once

#include <QByteArray>
#include <QImage>
#include <QString>

namespace Images {

    // PNG bytes from the container, decoded within bounds and scaled to fit
    // maxSide; a null image for anything else.
    QImage decode(const QByteArray &png, int maxSide);

    bool save(const QImage &image, const QString &path);

} // namespace Images
