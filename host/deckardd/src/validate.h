#pragma once

#include <QByteArray>
#include <QJsonObject>
#include <QString>
#include <QStringList>

#include <optional>

// Everything from the container is untrusted: these turn a bridge message
// into bounded, well-formed values or reject it (protocol/bridge.md).
namespace Validate {

    constexpr int kNoProgress = -2;

    bool    isPackageName(const QString &name);
    QString text(const QJsonObject &object, const QString &key, int maxLength);
    QByteArray base64(const QJsonObject &object, const QString &key, int maxBytes);
    QString fdoCategory(const QString &androidCategory);

    struct Post {
        QString     key;
        QString     pkg;
        QString     label;
        QString     title;
        QString     body;
        QString     category;
        QStringList actions; // Notify's flat id, label list
        QString     replyAction;
        QString     replyPlaceholder;
        QString     conversation; // x-marathon-conversation, compact JSON
        QByteArray  image;        // PNG bytes, unverified
        bool        ongoing    = false;
        bool        autoCancel = false;
        bool        silent     = false;
        int         urgency    = 1;
        int         progress   = kNoProgress; // percent, -1 indeterminate
    };
    std::optional<Post> post(const QJsonObject &message, QString *error);

    struct Media {
        int        sid = 0;
        QString    pkg;
        QString    label;
        QString    status;
        qint64     positionMs = 0;
        double     speed      = 1.0;
        qint64     durationMs = 0;
        QString    title;
        QString    artist;
        QString    album;
        bool       hasArt = false; // the message carries art; empty means none
        QByteArray art;
        bool       canPlay = false, canPause = false, canNext = false, canPrevious = false,
             canSeek = false, canStop = false;
    };
    std::optional<Media> media(const QJsonObject &message, QString *error);

} // namespace Validate
