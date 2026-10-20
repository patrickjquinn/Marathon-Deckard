#include "ime.h"

#include "validate.h"

#include <QDBusConnectionInterface>
#include <QDBusMessage>
#include <QDBusPendingCall>
#include <QDBusPendingCallWatcher>
#include <QDBusPendingReply>
#include <QLoggingCategory>

Q_LOGGING_CATEGORY(lcIme, "deckard.ime")

namespace {
    const QString kShell     = QStringLiteral("org.marathonos.Shell.Session");
    const QString kPath      = QStringLiteral("/org/marathonos/Shell/TextInput");
    const QString kInterface = QStringLiteral("org.marathonos.Shell.TextInput1");

    constexpr int  kMaxPurpose = 13;     // zwp_text_input_v3 terminal
    constexpr uint kHintMask   = 0x3ff;  // zwp_text_input_v3 none .. multiline

    bool isKey(const QString &name) {
        static const QStringList keys = {
            QStringLiteral("enter"), QStringLiteral("backspace"), QStringLiteral("left"),
            QStringLiteral("right"), QStringLiteral("up"),        QStringLiteral("down"),
            QStringLiteral("tab"),   QStringLiteral("escape"),
        };
        return keys.contains(name);
    }
} // namespace

Ime::Ime(Send toAndroid, QObject *parent)
    : QObject(parent)
    , m_toAndroid(std::move(toAndroid)) {}

void Ime::handle(const QString &type, const QJsonObject &message) {
    if (type == QLatin1String("start")) {
        const QString package = message.value(QStringLiteral("package")).toString();
        if (!Validate::isPackageName(package) || package.isEmpty()) {
            qCWarning(lcIme) << "start without a valid package";
            return;
        }
        m_appId            = kAppIdPrefix + package;
        const int  purpose = message.value(QStringLiteral("purpose")).toInt();
        const uint hints   = uint(message.value(QStringLiteral("hints")).toInt()) & kHintMask;
        Q_EMIT shellCall(QStringLiteral("SetEditor"),
                         {m_appId, uint(purpose >= 0 && purpose <= kMaxPurpose ? purpose : 0),
                          hints});
        sendPanel();
        return;
    }
    if (m_appId.isEmpty())
        return;
    if (type == QLatin1String("state")) {
        const QString text = message.value(QStringLiteral("text")).toString();
        if (text.size() > kMaxText)
            return;
        const int size   = int(text.size());
        const int cursor = qBound(0, message.value(QStringLiteral("cursor")).toInt(), size);
        const int anchor = qBound(0, message.value(QStringLiteral("anchor")).toInt(), size);
        Q_EMIT shellCall(QStringLiteral("SetSurroundingText"), {m_appId, text, cursor, anchor});
    } else if (type == QLatin1String("show")) {
        Q_EMIT shellCall(QStringLiteral("ShowPanel"), {m_appId});
    } else if (type == QLatin1String("hide")) {
        Q_EMIT shellCall(QStringLiteral("HidePanel"), {m_appId});
    } else if (type == QLatin1String("finish")) {
        Q_EMIT shellCall(QStringLiteral("ClearEditor"), {m_appId});
        m_appId.clear();
    }
}

void Ime::setConnected(bool connected) {
    if (connected || m_appId.isEmpty())
        return;
    Q_EMIT shellCall(QStringLiteral("HidePanel"), {m_appId});
    Q_EMIT shellCall(QStringLiteral("ClearEditor"), {m_appId});
    m_appId.clear();
}

void Ime::commitString(const QString &appId, const QString &text, int before, int after) {
    if (appId != m_appId || text.size() > kMaxText)
        return;
    m_toAndroid({{QStringLiteral("t"), QStringLiteral("commit")},
                 {QStringLiteral("text"), text},
                 {QStringLiteral("before"), qMax(0, before)},
                 {QStringLiteral("after"), qMax(0, after)}});
}

void Ime::key(const QString &appId, const QString &name) {
    if (appId != m_appId || !isKey(name))
        return;
    m_toAndroid({{QStringLiteral("t"), QStringLiteral("key")}, {QStringLiteral("key"), name}});
}

// Kept per app: the keyboard may be up before Android focuses an editor.
void Ime::panelChanged(const QString &appId, int covered, int height) {
    if (!appId.startsWith(kAppIdPrefix))
        return;
    m_panels.insert(appId, {qMax(0, covered), qMax(0, height)});
    if (appId == m_appId)
        sendPanel();
}

void Ime::sendPanel() {
    const auto it = m_panels.constFind(m_appId);
    if (it == m_panels.constEnd())
        return;
    m_toAndroid({{QStringLiteral("t"), QStringLiteral("panel")},
                 {QStringLiteral("covered"), it->first},
                 {QStringLiteral("height"), it->second}});
}

ShellTextInput::ShellTextInput(Ime *ime, const QDBusConnection &bus, QObject *parent)
    : QObject(parent)
    , m_ime(ime)
    , m_bus(bus)
    , m_watcher(kShell, bus, QDBusServiceWatcher::WatchForRegistration) {
    connect(ime, &Ime::shellCall, this, &ShellTextInput::call);
    m_bus.connect(kShell, kPath, kInterface, QStringLiteral("CommitString"), this,
                  SLOT(onCommitString(QString, QString, int, int)));
    m_bus.connect(kShell, kPath, kInterface, QStringLiteral("Key"), this,
                  SLOT(onKey(QString, QString)));
    m_bus.connect(kShell, kPath, kInterface, QStringLiteral("PanelChanged"), this,
                  SLOT(onPanelChanged(QString, int, int)));
    connect(&m_watcher, &QDBusServiceWatcher::serviceRegistered, this,
            &ShellTextInput::registerWithShell);
    if (m_bus.interface()->isServiceRegistered(kShell))
        registerWithShell();
}

void ShellTextInput::registerWithShell() {
    call(QStringLiteral("Register"), {Ime::kAppIdPrefix});
}

void ShellTextInput::call(const QString &method, const QVariantList &arguments) {
    QDBusMessage message = QDBusMessage::createMethodCall(kShell, kPath, kInterface, method);
    message.setArguments(arguments);
    auto *watcher = new QDBusPendingCallWatcher(m_bus.asyncCall(message), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this,
            [method](QDBusPendingCallWatcher *w) {
                const QDBusPendingReply<> reply = *w;
                if (reply.isError())
                    qCWarning(lcIme) << method << "failed:" << reply.error().message();
                w->deleteLater();
            });
}

void ShellTextInput::onCommitString(const QString &appId, const QString &text, int before,
                                    int after) {
    m_ime->commitString(appId, text, before, after);
}

void ShellTextInput::onKey(const QString &appId, const QString &name) {
    m_ime->key(appId, name);
}

void ShellTextInput::onPanelChanged(const QString &appId, int covered, int height) {
    m_ime->panelChanged(appId, covered, height);
}
