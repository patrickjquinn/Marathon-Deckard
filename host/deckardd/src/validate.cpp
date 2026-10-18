#include "validate.h"

#include <QJsonArray>
#include <QJsonDocument>
#include <QRegularExpression>

#include <cmath>

namespace Validate {

    namespace {

        bool isActionId(const QString &id) {
            static const QRegularExpression kId(QStringLiteral("^a[0-9]{1,2}$"));
            return kId.match(id).hasMatch();
        }

        qint64 integer(const QJsonObject &object, const QString &key, qint64 min, qint64 max,
                       qint64 fallback) {
            const double value = object.value(key).toDouble(double(fallback));
            if (!std::isfinite(value))
                return fallback;
            return qBound(min, qint64(value), max);
        }

        QString conversation(const QJsonObject &c) {
            QJsonArray messages;
            const QJsonArray in = c.value(QStringLiteral("messages")).toArray();
            for (qsizetype i = qMax<qsizetype>(0, in.size() - 10); i < in.size(); ++i) {
                const QJsonObject m = in.at(i).toObject();
                messages.append(QJsonObject{
                    {QStringLiteral("sender"), text(m, QStringLiteral("sender"), 128)},
                    {QStringLiteral("text"), text(m, QStringLiteral("text"), 1024)},
                    {QStringLiteral("time"), double(integer(m, QStringLiteral("time"), 0,
                                                            Q_INT64_C(1) << 50, 0))}});
            }
            const QJsonObject out{
                {QStringLiteral("title"), text(c, QStringLiteral("title"), 256)},
                {QStringLiteral("self"), text(c, QStringLiteral("self"), 128)},
                {QStringLiteral("group"), c.value(QStringLiteral("group")).toBool()},
                {QStringLiteral("messages"), messages}};
            return QString::fromUtf8(QJsonDocument(out).toJson(QJsonDocument::Compact));
        }

    } // namespace

    bool isPackageName(const QString &name) {
        static const QRegularExpression kPackage(
            QStringLiteral("^[A-Za-z][A-Za-z0-9_]*(\\.[A-Za-z][A-Za-z0-9_]*)*$"));
        return name.size() <= 255 && kPackage.match(name).hasMatch();
    }

    QString text(const QJsonObject &object, const QString &key, int maxLength) {
        const QString raw = object.value(key).toString();
        QString       out;
        out.reserve(qMin<qsizetype>(raw.size(), maxLength));
        for (const QChar c : raw) {
            if (out.size() >= maxLength)
                break;
            if (c == u'\n' || c == u'\t' || (c.category() != QChar::Other_Control &&
                                              c.category() != QChar::Other_Format))
                out.append(c);
        }
        if (!out.isEmpty() && out.back().isHighSurrogate())
            out.chop(1);
        return out;
    }

    QByteArray base64(const QJsonObject &object, const QString &key, int maxBytes) {
        const QString encoded = object.value(key).toString();
        if (encoded.isEmpty() || encoded.size() > (qsizetype(maxBytes) + 2) / 3 * 4)
            return {};
        const auto decoded = QByteArray::fromBase64Encoding(
            encoded.toLatin1(), QByteArray::AbortOnBase64DecodingErrors);
        return decoded ? *decoded : QByteArray();
    }

    QString fdoCategory(const QString &android) {
        static const QHash<QString, QString> kMap{
            {QStringLiteral("msg"), QStringLiteral("im.received")},
            {QStringLiteral("email"), QStringLiteral("email.arrived")},
            {QStringLiteral("call"), QStringLiteral("call.incoming")},
            {QStringLiteral("missed_call"), QStringLiteral("call.unanswered")},
            {QStringLiteral("progress"), QStringLiteral("transfer")},
            {QStringLiteral("social"), QStringLiteral("presence")},
            {QStringLiteral("err"), QStringLiteral("device.error")},
            {QStringLiteral("sys"), QStringLiteral("device")}};
        if (android.isEmpty())
            return {};
        static const QRegularExpression kWord(QStringLiteral("^[a-z_]{1,32}$"));
        if (const auto it = kMap.constFind(android); it != kMap.constEnd())
            return it.value();
        return kWord.match(android).hasMatch() ? QStringLiteral("x-android.") + android : QString();
    }

    std::optional<Post> post(const QJsonObject &m, QString *error) {
        Post p;
        p.key = m.value(QStringLiteral("key")).toString();
        if (p.key.isEmpty() || p.key.size() > 512 || text(m, QStringLiteral("key"), 512) != p.key) {
            *error = QStringLiteral("bad key");
            return std::nullopt;
        }
        p.pkg = m.value(QStringLiteral("pkg")).toString();
        if (!isPackageName(p.pkg)) {
            *error = QStringLiteral("bad package");
            return std::nullopt;
        }
        p.label = text(m, QStringLiteral("label"), 128);
        if (p.label.trimmed().isEmpty())
            p.label = p.pkg;
        p.title      = text(m, QStringLiteral("title"), 256);
        p.body       = text(m, QStringLiteral("text"), 4096);
        p.category   = fdoCategory(text(m, QStringLiteral("category"), 64));
        p.ongoing    = m.value(QStringLiteral("ongoing")).toBool();
        p.autoCancel = m.value(QStringLiteral("autoCancel")).toBool();
        p.silent     = m.value(QStringLiteral("silent")).toBool();
        p.urgency    = int(integer(m, QStringLiteral("urgency"), 0, 2, 1));

        if (m.value(QStringLiteral("content")).toBool())
            p.actions << QStringLiteral("default") << QStringLiteral("Open");
        const QJsonObject reply = m.value(QStringLiteral("reply")).toObject();
        const QString     replyAction = reply.value(QStringLiteral("action")).toString();
        if (isActionId(replyAction)) {
            p.replyAction      = replyAction;
            p.replyPlaceholder = text(reply, QStringLiteral("placeholder"), 128);
            QString label      = text(reply, QStringLiteral("label"), 64);
            p.actions << QStringLiteral("inline-reply")
                      << (label.trimmed().isEmpty() ? QStringLiteral("Reply") : label);
        }
        const QJsonArray actions = m.value(QStringLiteral("actions")).toArray();
        int              added   = 0;
        for (const QJsonValue &value : actions) {
            const QJsonObject a     = value.toObject();
            const QString     id    = a.value(QStringLiteral("id")).toString();
            const QString     label = text(a, QStringLiteral("label"), 64);
            if (!isActionId(id) || id == p.replyAction || label.trimmed().isEmpty() ||
                p.actions.contains(id))
                continue;
            p.actions << id << label;
            if (++added == 5)
                break;
        }

        if (m.contains(QStringLiteral("progress"))) {
            const QJsonObject progress = m.value(QStringLiteral("progress")).toObject();
            const qint64      max = integer(progress, QStringLiteral("max"), 0, INT_MAX, 0);
            const qint64 current  = integer(progress, QStringLiteral("current"), 0, INT_MAX, 0);
            if (progress.value(QStringLiteral("indeterminate")).toBool())
                p.progress = -1;
            else if (max > 0)
                p.progress = int(qBound(qint64(0), current * 100 / max, qint64(100)));
        }
        if (m.value(QStringLiteral("conversation")).isObject())
            p.conversation = conversation(m.value(QStringLiteral("conversation")).toObject());
        p.image = base64(m, QStringLiteral("image"), 512 * 1024);
        return p;
    }

    std::optional<Media> media(const QJsonObject &m, QString *error) {
        Media s;
        s.sid = int(integer(m, QStringLiteral("sid"), 0, INT_MAX, 0));
        s.pkg = m.value(QStringLiteral("pkg")).toString();
        if (s.sid <= 0 || !isPackageName(s.pkg)) {
            *error = QStringLiteral("bad session");
            return std::nullopt;
        }
        s.label = text(m, QStringLiteral("label"), 128);
        if (s.label.trimmed().isEmpty())
            s.label = s.pkg;
        const QString state = m.value(QStringLiteral("state")).toString();
        s.status = state == QLatin1String("Playing") || state == QLatin1String("Paused")
                       ? state
                       : QStringLiteral("Stopped");
        constexpr qint64 kMaxMs = Q_INT64_C(100000000000);
        s.positionMs = integer(m, QStringLiteral("position"), 0, kMaxMs, 0);
        s.durationMs = integer(m, QStringLiteral("duration"), 0, kMaxMs, 0);
        const double speed = m.value(QStringLiteral("speed")).toDouble(1.0);
        s.speed  = std::isfinite(speed) ? qBound(-16.0, speed, 16.0) : 1.0;
        s.title  = text(m, QStringLiteral("title"), 256);
        s.artist = text(m, QStringLiteral("artist"), 256);
        s.album  = text(m, QStringLiteral("album"), 256);
        s.hasArt = m.contains(QStringLiteral("art"));
        if (s.hasArt)
            s.art = base64(m, QStringLiteral("art"), 768 * 1024);
        const QJsonObject can = m.value(QStringLiteral("can")).toObject();
        s.canPlay     = can.value(QStringLiteral("play")).toBool();
        s.canPause    = can.value(QStringLiteral("pause")).toBool();
        s.canNext     = can.value(QStringLiteral("next")).toBool();
        s.canPrevious = can.value(QStringLiteral("previous")).toBool();
        s.canSeek     = can.value(QStringLiteral("seek")).toBool();
        s.canStop     = can.value(QStringLiteral("stop")).toBool();
        return s;
    }

} // namespace Validate
