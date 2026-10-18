#include "images.h"

#include <QBuffer>
#include <QDir>
#include <QFileInfo>
#include <QImageReader>
#include <QSaveFile>

namespace Images {

    QImage decode(const QByteArray &png, int maxSide) {
        static const QByteArray kSignature("\x89PNG\r\n\x1a\n", 8);
        if (!png.startsWith(kSignature))
            return {};
        QBuffer buffer;
        buffer.setData(png);
        buffer.open(QIODevice::ReadOnly);
        QImageReader reader(&buffer, "png");
        const QSize  size = reader.size();
        if (!size.isValid() || size.width() > 4096 || size.height() > 4096)
            return {};
        if (qMax(size.width(), size.height()) > maxSide)
            reader.setScaledSize(size.scaled(maxSide, maxSide, Qt::KeepAspectRatio));
        const QImage image = reader.read();
        return image.isNull() ? QImage() : image.convertToFormat(QImage::Format_RGBA8888);
    }

    bool save(const QImage &image, const QString &path) {
        QDir().mkpath(QFileInfo(path).absolutePath());
        QSaveFile file(path);
        return file.open(QIODevice::WriteOnly) && image.save(&file, "PNG") && file.commit();
    }

} // namespace Images
