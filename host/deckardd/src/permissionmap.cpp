#include "permissionmap.h"

#include <QHash>
#include <QJsonObject>

namespace PermissionMap {

    namespace {
        struct Entry {
            QString     name;
            QStringList android;
        };

        QString a(const char *p) {
            return QStringLiteral("android.permission.") + QLatin1String(p);
        }

        const QList<Entry> &table() {
            static const QList<Entry> kTable = {
                {QStringLiteral("location"), {a("ACCESS_FINE_LOCATION"), a("ACCESS_COARSE_LOCATION"), a("ACCESS_BACKGROUND_LOCATION")}},
                {QStringLiteral("camera"), {a("CAMERA")}},
                {QStringLiteral("microphone"), {a("RECORD_AUDIO")}},
                {QStringLiteral("contacts"), {a("READ_CONTACTS"), a("WRITE_CONTACTS"), a("GET_ACCOUNTS")}},
                {QStringLiteral("calendar"), {a("READ_CALENDAR"), a("WRITE_CALENDAR")}},
                {QStringLiteral("storage"), {a("READ_EXTERNAL_STORAGE"), a("WRITE_EXTERNAL_STORAGE"), a("READ_MEDIA_IMAGES"),
                                             a("READ_MEDIA_VIDEO"), a("READ_MEDIA_AUDIO")}},
                {QStringLiteral("notifications"), {a("POST_NOTIFICATIONS")}},
                {QStringLiteral("telephony"), {a("READ_PHONE_STATE"), a("READ_PHONE_NUMBERS"), a("CALL_PHONE"),
                                               a("ANSWER_PHONE_CALLS"), a("ADD_VOICEMAIL"), a("USE_SIP"), a("ACCEPT_HANDOVER"),
                                               a("READ_CALL_LOG"), a("WRITE_CALL_LOG"), a("PROCESS_OUTGOING_CALLS")}},
                {QStringLiteral("sms"), {a("SEND_SMS"), a("RECEIVE_SMS"), a("READ_SMS"), a("RECEIVE_WAP_PUSH"), a("RECEIVE_MMS")}},
                {QStringLiteral("bluetooth"), {a("BLUETOOTH_SCAN"), a("BLUETOOTH_CONNECT"), a("BLUETOOTH_ADVERTISE"),
                                               a("NEARBY_WIFI_DEVICES"), a("UWB_RANGING")}},
                {QStringLiteral("sensors"), {a("BODY_SENSORS"), a("BODY_SENSORS_BACKGROUND"), a("ACTIVITY_RECOGNITION")}},
            };
            return kTable;
        }
    } // namespace

    const QStringList &names() {
        static const QStringList kNames = [] {
            QStringList out;
            for (const Entry &e : table())
                out << e.name;
            return out;
        }();
        return kNames;
    }

    bool isName(const QString &name) {
        return names().contains(name);
    }

    QStringList android(const QString &name) {
        for (const Entry &e : table())
            if (e.name == name)
                return e.android;
        return {};
    }

    QString marathon(const QString &androidPermission) {
        static const QHash<QString, QString> kBack = [] {
            QHash<QString, QString> out;
            for (const Entry &e : table())
                for (const QString &p : e.android)
                    out.insert(p, e.name);
            return out;
        }();
        return kBack.value(androidPermission);
    }

    QJsonArray toJson() {
        QJsonArray out;
        for (const Entry &e : table())
            out.append(QJsonObject{{QStringLiteral("name"), e.name}, {QStringLiteral("android"), QJsonArray::fromStringList(e.android)}});
        return out;
    }

} // namespace PermissionMap
