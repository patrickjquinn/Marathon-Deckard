#include "packages.h"

#include "validate.h"

#include <QDBusConnection>
#include <QDBusMetaType>
#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QJsonArray>
#include <QLoggingCategory>
#include <QRandomGenerator>
#include <QTimer>

#include <sys/stat.h>

Q_LOGGING_CATEGORY(lcPackages, "deckard.packages")

Q_DECLARE_METATYPE(PackageList)

namespace {
    const QString kAndroidDir = QStringLiteral("/data/deckard/install/");

    QString newId() {
        return QString::number(QRandomGenerator::global()->generate64(), 16);
    }

    QVariantMap failure(const QString &message) {
        return {{QStringLiteral("ok"), false}, {QStringLiteral("message"), message}};
    }
} // namespace

Packages::Packages(QObject *root, Send send, MayChange mayChange, const QString &installDir)
    : QDBusAbstractAdaptor(root)
    , m_send(std::move(send))
    , m_mayChange(std::move(mayChange))
    , m_dir(installDir) {
    qDBusRegisterMetaType<PackageList>();
    QDir().mkpath(m_dir);
    QFile::setPermissions(m_dir, QFile::ReadOwner | QFile::WriteOwner | QFile::ExeOwner |
                                     QFile::ReadGroup | QFile::ExeGroup | QFile::ReadOther |
                                     QFile::ExeOther);
    const QStringList left = QDir(m_dir).entryList({QStringLiteral("*.apk")}, QDir::Files);
    for (const QString &name : left)
        QFile::remove(m_dir + u'/' + name);
}

bool Packages::runs(qint64 pid, const QString &path) {
    struct stat exe {};
    struct stat file {};
    const QByteArray link = "/proc/" + QByteArray::number(pid) + "/exe";
    return pid > 0 && ::stat(link.constData(), &exe) == 0 &&
           ::stat(QFile::encodeName(path).constData(), &file) == 0 && exe.st_dev == file.st_dev &&
           exe.st_ino == file.st_ino;
}

void Packages::sendReply(const QDBusMessage &reply) {
    QDBusConnection::sessionBus().send(reply);
}

QString Packages::stage(const QString &path, QString *error) {
    const QFileInfo info(path);
    if (!info.isFile() || info.size() <= 4 || info.size() > kMaxApk) {
        *error = QStringLiteral("not a file");
        return {};
    }
    QFile in(path);
    if (!in.open(QIODevice::ReadOnly) || in.read(4) != QByteArrayLiteral("PK\x03\x04")) {
        *error = QStringLiteral("not an Android app");
        return {};
    }
    in.close();
    const QString name   = newId() + QStringLiteral(".apk");
    const QString staged = m_dir + u'/' + name;
    if (!QFile::copy(path, staged)) {
        *error = QStringLiteral("cannot copy the app into Android");
        return {};
    }
    QFile::setPermissions(staged, QFile::ReadOwner | QFile::WriteOwner | QFile::ReadGroup |
                                      QFile::ReadOther);
    return name;
}

QString Packages::start(const QJsonObject &message, const Pending &pending, QString *error) {
    const QString id = message.value(QStringLiteral("id")).toString();
    if (!m_send(message)) {
        if (!pending.staged.isEmpty())
            QFile::remove(m_dir + u'/' + pending.staged);
        *error = QStringLiteral("Android isn't running");
        return {};
    }
    m_pending.insert(id, pending);
    const int timeout = pending.type == QLatin1String("install") ? 180000 : 30000;
    QTimer::singleShot(timeout, this, [this, id] {
        if (m_pending.contains(id))
            done(id, false, {}, QStringLiteral("Android didn't answer"));
    });
    return id;
}

void Packages::reply(const QString &id, const QVariant &value) {
    const Pending p = m_pending.take(id);
    if (!p.staged.isEmpty())
        QFile::remove(m_dir + u'/' + p.staged);
    sendReply(p.call.createReply(value));
}

void Packages::done(const QString &id, bool ok, const QString &pkg, const QString &message) {
    const auto it = m_pending.constFind(id);
    if (it == m_pending.constEnd())
        return;
    if (it->call.type() == QDBusMessage::MethodCallMessage) {
        reply(id, failure(message));
        return;
    }
    const Pending p = m_pending.take(id);
    if (!p.staged.isEmpty())
        QFile::remove(m_dir + u'/' + p.staged);
    qCInfo(lcPackages) << p.type << pkg << (ok ? "done" : "failed:") << message;
    Q_EMIT Finished(id, ok, pkg, message);
}

QVariantMap Packages::InspectPackage(const QString &path, const QDBusMessage &call) {
    QString error;
    const QString staged = stage(path, &error);
    if (staged.isEmpty())
        return failure(error);
    const QString id = newId();
    if (start({{QStringLiteral("t"), QStringLiteral("inspect")},
               {QStringLiteral("id"), id},
               {QStringLiteral("path"), kAndroidDir + staged}},
              {call, QStringLiteral("inspect"), staged}, &error)
            .isEmpty())
        return failure(error);
    call.setDelayedReply(true);
    return {};
}

void Packages::startJob(const QDBusMessage &call, const QJsonObject &message, const Pending &pending) {
    QString       error;
    const QString id = start(message, pending, &error);
    sendReply(id.isEmpty() ? call.createErrorReply(QDBusError::Failed, error) : call.createReply(id));
}

QString Packages::InstallPackage(const QString &path, const QDBusMessage &call) {
    call.setDelayedReply(true);
    m_mayChange(call, [this, path, call] {
        QString       error;
        const QString staged = stage(path, &error);
        if (staged.isEmpty()) {
            sendReply(call.createErrorReply(QDBusError::Failed, error));
            return;
        }
        qCInfo(lcPackages) << "installing" << path;
        startJob(call,
                 {{QStringLiteral("t"), QStringLiteral("install")},
                  {QStringLiteral("id"), newId()},
                  {QStringLiteral("path"), kAndroidDir + staged}},
                 {{}, QStringLiteral("install"), staged});
    });
    return {};
}

QString Packages::UninstallPackage(const QString &pkg, const QDBusMessage &call) {
    call.setDelayedReply(true);
    m_mayChange(call, [this, pkg, call] {
        if (!Validate::isPackageName(pkg)) {
            sendReply(call.createErrorReply(QDBusError::InvalidArgs, QStringLiteral("bad package name")));
            return;
        }
        startJob(call,
                 {{QStringLiteral("t"), QStringLiteral("uninstall")},
                  {QStringLiteral("id"), newId()},
                  {QStringLiteral("pkg"), pkg}},
                 {{}, QStringLiteral("uninstall"), {}});
    });
    return {};
}

PackageList Packages::ListPackages(const QDBusMessage &call) {
    QString error;
    if (start({{QStringLiteral("t"), QStringLiteral("packages")}, {QStringLiteral("id"), newId()}},
              {call, QStringLiteral("packages"), {}}, &error)
            .isEmpty()) {
        sendReply(call.createErrorReply(QDBusError::Failed, error));
        return {};
    }
    call.setDelayedReply(true);
    return {};
}

void Packages::handle(const QString &type, const QJsonObject &m) {
    if (type == QLatin1String("package_changed")) {
        const QString pkg    = m.value(QStringLiteral("pkg")).toString();
        const QString change = m.value(QStringLiteral("change")).toString();
        if (Validate::isPackageName(pkg) &&
            (change == QLatin1String("installed") || change == QLatin1String("updated") ||
             change == QLatin1String("removed")))
            Q_EMIT PackageChanged(pkg, change);
        return;
    }
    const QString id = m.value(QStringLiteral("id")).toString();
    if (!m_pending.contains(id))
        return;
    if (type == QLatin1String("install_progress")) {
        Q_EMIT Progress(id, qBound(0, m.value(QStringLiteral("percent")).toInt(), 100));
        return;
    }
    if (type == QLatin1String("packages_result")) {
        PackageList list;
        const QJsonArray packages = m.value(QStringLiteral("packages")).toArray();
        for (const QJsonValue &v : packages) {
            const QJsonObject o   = v.toObject();
            const QString     pkg = o.value(QStringLiteral("pkg")).toString();
            if (!Validate::isPackageName(pkg))
                continue;
            list.append({{QStringLiteral("pkg"), pkg},
                         {QStringLiteral("label"), Validate::label(o, QStringLiteral("label"))},
                         {QStringLiteral("versionName"), Validate::text(o, QStringLiteral("versionName"), 64)},
                         {QStringLiteral("versionCode"), qint64(o.value(QStringLiteral("versionCode")).toDouble())},
                         {QStringLiteral("system"), o.value(QStringLiteral("system")).toBool()},
                         {QStringLiteral("launchable"), o.value(QStringLiteral("launchable")).toBool(true)}});
            if (list.size() >= 2000)
                break;
        }
        reply(id, QVariant::fromValue(list));
        return;
    }
    const bool    ok      = m.value(QStringLiteral("ok")).toBool();
    const QString message = Validate::text(m, QStringLiteral("message"), 512);
    QString       pkg     = m.value(QStringLiteral("pkg")).toString();
    if (!Validate::isPackageName(pkg))
        pkg.clear();
    if (type == QLatin1String("install_result")) {
        done(id, ok, pkg, message);
        return;
    }
    if (type != QLatin1String("inspect_result"))
        return;
    if (!ok) {
        reply(id, failure(message));
        return;
    }
    QVariantMap info{{QStringLiteral("ok"), true},
                     {QStringLiteral("pkg"), pkg},
                     {QStringLiteral("label"), Validate::label(m, QStringLiteral("label"))},
                     {QStringLiteral("versionName"), Validate::text(m, QStringLiteral("versionName"), 64)},
                     {QStringLiteral("versionCode"), qint64(m.value(QStringLiteral("versionCode")).toDouble())},
                     {QStringLiteral("minSdk"), m.value(QStringLiteral("minSdk")).toInt()},
                     {QStringLiteral("targetSdk"), m.value(QStringLiteral("targetSdk")).toInt()}};
    QStringList permissions;
    const QJsonArray perms = m.value(QStringLiteral("permissions")).toArray();
    for (const QJsonValue &p : perms) {
        const QString name = p.toString();
        if (name.size() <= 160 && Validate::isPackageName(name))
            permissions.append(name);
        if (permissions.size() >= 200)
            break;
    }
    info.insert(QStringLiteral("permissions"), permissions);
    info.insert(QStringLiteral("icon"), Validate::base64(m, QStringLiteral("icon"), 256 * 1024));
    if (m.contains(QStringLiteral("installedVersionCode"))) {
        info.insert(QStringLiteral("installedVersionCode"),
                    qint64(m.value(QStringLiteral("installedVersionCode")).toDouble()));
        info.insert(QStringLiteral("installedVersionName"),
                    Validate::text(m, QStringLiteral("installedVersionName"), 64));
    }
    reply(id, info);
}

void Packages::bridgeLost() {
    const QStringList ids = m_pending.keys();
    for (const QString &id : ids)
        done(id, false, {}, QStringLiteral("Android stopped"));
}
