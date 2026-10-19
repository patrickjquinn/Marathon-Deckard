#pragma once

#include <QJsonArray>
#include <QStringList>

// Android runtime permissions by Marathon permission name (Marathon-Shell
// schemas/marathon-permissions-v1.json): the one table Settings, the
// permission prompts and DeckardSystem's grants go by. "notifications" is
// Android's per-app notification switch rather than these grants.
namespace PermissionMap {

    // The Marathon permissions Android apps can hold, in Settings' order.
    const QStringList &names();
    bool               isName(const QString &name);
    QStringList        android(const QString &name);
    // The Marathon permission an Android permission belongs to, or "".
    QString            marathon(const QString &androidPermission);
    // As JSON, for the protocol document and DeckardSystem.
    QJsonArray         toJson();

} // namespace PermissionMap
