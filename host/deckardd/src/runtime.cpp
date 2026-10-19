#include "runtime.h"

#include <QDBusArgument>
#include <QDBusMessage>
#include <QDBusObjectPath>
#include <QDBusPendingCallWatcher>
#include <QDBusVariant>
#include <QDateTime>
#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QJsonDocument>
#include <QJsonObject>
#include <QLoggingCategory>
#include <QProcess>
#include <QTimer>
#include <QSaveFile>

#include <memory>

Q_LOGGING_CATEGORY(lcRuntime, "deckard.runtime")

namespace {
    const QString kStartAtLogin = QStringLiteral("startAtLogin");

    QJsonObject readPolicy(const QString &path) {
        QFile f(path);
        if (!f.open(QIODevice::ReadOnly))
            return {};
        return QJsonDocument::fromJson(f.readAll()).object();
    }
} // namespace

Runtime::Runtime(Paths paths, UnitCall unitCall, QObject *parent)
    : QObject(parent)
    , m_paths(std::move(paths))
    , m_unitCall(std::move(unitCall)) {
    if (m_paths.reset.isEmpty()) {
        const QString helper = qEnvironmentVariable("DECKARD_RESET_HELPER");
        if (!helper.isEmpty())
            m_paths.reset = {helper};
        else if (QFileInfo::exists(QStringLiteral("/usr/libexec/deckard-reset")))
            m_paths.reset = {QStringLiteral("pkexec"), QStringLiteral("/usr/libexec/deckard-reset")};
    }
}

QString Runtime::state() const {
    if (m_unitState == QLatin1String("active") || m_unitState == QLatin1String("reloading"))
        return m_android ? QStringLiteral("running") : QStringLiteral("starting");
    if (m_unitState == QLatin1String("activating"))
        return QStringLiteral("starting");
    if (m_unitState == QLatin1String("deactivating"))
        return QStringLiteral("stopping");
    return QStringLiteral("stopped");
}

qint64 Runtime::activeSinceMs() const {
    return m_unitState == QLatin1String("active") || m_unitState == QLatin1String("reloading") ? m_enteredMs : 0;
}

QVariantMap Runtime::snapshot() const {
    const QString s = state();
    qint64        memory = 0;
    if (s != QLatin1String("stopped")) {
        QFile f(m_paths.memory);
        if (f.open(QIODevice::ReadOnly))
            memory = qMax<qint64>(0, f.readAll().trimmed().toLongLong());
    }
    const qint64 uptime = s == QLatin1String("running") && m_enteredMs > 0
                              ? qMax<qint64>(0, (QDateTime::currentMSecsSinceEpoch() - m_enteredMs) / 1000)
                              : 0;
    return {{QStringLiteral("state"), s},
            {QStringLiteral("memoryBytes"), memory},
            {QStringLiteral("androidVersion"), m_version},
            {QStringLiteral("uptimeSeconds"), uptime},
            {kStartAtLogin, startAtLogin()},
            {QStringLiteral("hasApps"), hasAndroidApps()}};
}

bool Runtime::startAtLoginIsSet() const {
    return readPolicy(m_paths.policy).contains(kStartAtLogin);
}

bool Runtime::startAtLogin() const {
    const QJsonObject policy = readPolicy(m_paths.policy);
    if (policy.contains(kStartAtLogin))
        return policy.value(kStartAtLogin).toBool();
    return hasAndroidApps();
}

// Waydroid writes a desktop entry for every launchable Android app; the
// image's own apps are NoDisplay.
bool Runtime::hasAndroidApps() const {
    const QFileInfoList entries = QDir(m_paths.applications)
                                      .entryInfoList({QStringLiteral("waydroid.*.desktop")}, QDir::Files);
    for (const QFileInfo &entry : entries) {
        QFile f(entry.filePath());
        if (!f.open(QIODevice::ReadOnly))
            continue;
        if (!f.read(64 * 1024).contains("\nNoDisplay=true"))
            return true;
    }
    return false;
}

void Runtime::start() {
    m_unitCall(QStringLiteral("StartUnit"));
}

void Runtime::stop() {
    m_unitCall(QStringLiteral("StopUnit"));
}

void Runtime::restart() {
    m_unitCall(QStringLiteral("RestartUnit"));
}

void Runtime::setStartAtLogin(bool on) {
    QJsonObject policy = readPolicy(m_paths.policy);
    policy.insert(kStartAtLogin, on);
    QDir().mkpath(QFileInfo(m_paths.policy).absolutePath());
    QSaveFile f(m_paths.policy);
    if (!f.open(QIODevice::WriteOnly) || f.write(QJsonDocument(policy).toJson()) < 0 || !f.commit())
        qCWarning(lcRuntime) << "cannot save" << m_paths.policy;
    Q_EMIT changed();
}

void Runtime::atLogin() {
    if (startAtLogin() && state() == QLatin1String("stopped")) {
        qCInfo(lcRuntime) << "starting Android at login";
        start();
    }
}

void Runtime::reset(Done done) {
    if (m_resetting) {
        done(false, QStringLiteral("Android is already being reset"));
        return;
    }
    if (m_paths.reset.isEmpty()) {
        done(false, QStringLiteral("Resetting Android isn't available"));
        return;
    }
    m_resetting = true;
    if (state() == QLatin1String("stopped")) {
        runReset(std::move(done));
        return;
    }
    auto *waiter = new QObject(this);
    auto  shared = std::make_shared<Done>(std::move(done));
    connect(this, &Runtime::changed, waiter, [this, waiter, shared] {
        if (state() != QLatin1String("stopped"))
            return;
        waiter->deleteLater();
        runReset(*shared);
    });
    QTimer::singleShot(kStopTimeoutMs, waiter, [this, waiter, shared] {
        waiter->deleteLater();
        m_resetting = false;
        (*shared)(false, QStringLiteral("Android didn't stop"));
    });
    stop();
}

void Runtime::runReset(Done done) {
    auto *helper = new QProcess(this);
    auto  shared = std::make_shared<Done>(std::move(done));
    connect(helper, &QProcess::finished, this, [this, helper, shared](int code, QProcess::ExitStatus status) {
        helper->deleteLater();
        m_resetting = false;
        const bool ok = status == QProcess::NormalExit && code == 0;
        if (!ok)
            qCWarning(lcRuntime) << "reset helper failed:" << code << helper->readAllStandardError().trimmed();
        (*shared)(ok, ok ? QString() : QStringLiteral("Android's data couldn't be removed"));
    });
    connect(helper, &QProcess::errorOccurred, this, [this, helper, shared](QProcess::ProcessError error) {
        if (error != QProcess::FailedToStart)
            return;
        helper->deleteLater();
        m_resetting = false;
        (*shared)(false, QStringLiteral("Resetting Android isn't available"));
    });
    qCInfo(lcRuntime) << "resetting Android";
    helper->start(m_paths.reset.first(), m_paths.reset.mid(1));
}

void Runtime::setUnit(const QString &activeState, qint64 enteredMs) {
    if (activeState == m_unitState && enteredMs == m_enteredMs)
        return;
    m_unitState = activeState;
    m_enteredMs = enteredMs;
    Q_EMIT changed();
}

void Runtime::setAndroidConnected(bool connected) {
    if (connected == m_android)
        return;
    m_android = connected;
    Q_EMIT changed();
}

void Runtime::setAndroidVersion(const QString &release) {
    if (release == m_version)
        return;
    m_version = release;
    Q_EMIT changed();
}

UnitWatch::UnitWatch(QDBusConnection bus, const QString &unit, Runtime *runtime, QObject *parent)
    : QObject(parent)
    , m_bus(bus)
    , m_unit(unit)
    , m_runtime(runtime) {
    m_bus.call(QDBusMessage::createMethodCall(
                   QStringLiteral("org.freedesktop.systemd1"), QStringLiteral("/org/freedesktop/systemd1"),
                   QStringLiteral("org.freedesktop.systemd1.Manager"), QStringLiteral("Subscribe")),
               QDBus::NoBlock);
    QDBusMessage load = QDBusMessage::createMethodCall(
        QStringLiteral("org.freedesktop.systemd1"), QStringLiteral("/org/freedesktop/systemd1"),
        QStringLiteral("org.freedesktop.systemd1.Manager"), QStringLiteral("LoadUnit"));
    load << unit;
    auto *watcher = new QDBusPendingCallWatcher(m_bus.asyncCall(load), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [this, watcher] {
        watcher->deleteLater();
        const QDBusMessage reply = watcher->reply();
        if (reply.type() != QDBusMessage::ReplyMessage || reply.arguments().isEmpty()) {
            qCWarning(lcRuntime) << "LoadUnit" << m_unit << reply.errorMessage();
            return;
        }
        m_path = qvariant_cast<QDBusObjectPath>(reply.arguments().first()).path();
        m_bus.connect(QStringLiteral("org.freedesktop.systemd1"), m_path,
                      QStringLiteral("org.freedesktop.DBus.Properties"),
                      QStringLiteral("PropertiesChanged"), this,
                      SLOT(onPropertiesChanged(QString, QVariantMap, QStringList)));
        refresh();
    });
}

void UnitWatch::call(QDBusConnection bus, const QString &method, const QString &unit) {
    QDBusMessage m = QDBusMessage::createMethodCall(
        QStringLiteral("org.freedesktop.systemd1"), QStringLiteral("/org/freedesktop/systemd1"),
        QStringLiteral("org.freedesktop.systemd1.Manager"), method);
    m << unit << QStringLiteral("replace");
    bus.send(m);
}

void UnitWatch::onPropertiesChanged(const QString &interface, const QVariantMap &,
                                    const QStringList &) {
    if (interface == QLatin1String("org.freedesktop.systemd1.Unit"))
        refresh();
}

void UnitWatch::refresh() {
    QDBusMessage get = QDBusMessage::createMethodCall(QStringLiteral("org.freedesktop.systemd1"), m_path,
                                                      QStringLiteral("org.freedesktop.DBus.Properties"),
                                                      QStringLiteral("GetAll"));
    get << QStringLiteral("org.freedesktop.systemd1.Unit");
    auto *watcher = new QDBusPendingCallWatcher(m_bus.asyncCall(get), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [this, watcher] {
        watcher->deleteLater();
        const QDBusMessage reply = watcher->reply();
        if (reply.type() != QDBusMessage::ReplyMessage || reply.arguments().isEmpty())
            return;
        const QVariantMap props = qdbus_cast<QVariantMap>(reply.arguments().first());
        const qint64      enteredUs = props.value(QStringLiteral("ActiveEnterTimestamp")).toLongLong();
        m_runtime->setUnit(props.value(QStringLiteral("ActiveState")).toString(), enteredUs / 1000);
    });
}
