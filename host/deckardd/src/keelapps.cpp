#include "keelapps.h"

#include "validate.h"

#include <QCoreApplication>
#include <QDBusConnectionInterface>
#include <QDBusPendingCallWatcher>
#include <QDBusReply>
#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QJsonArray>
#include <QJsonDocument>
#include <QLoggingCategory>
#include <QRegularExpression>
#include <QSaveFile>
#include <QSet>
#include <QTimer>
#include <QUrl>

#include <sys/stat.h>

Q_LOGGING_CATEGORY(lcKeel, "deckard.keel")

namespace {

    const QString kPath      = QStringLiteral("/org/shipwright/Keel/Actions");
    const QString kInterface = QStringLiteral("org.shipwright.Keel.Actions");
    const QString kErrors    = QStringLiteral("org.shipwright.Keel.Actions.Error.");
    const QString kMeta      = QStringLiteral("org.shipwright.keel/");

    QString compact(const QJsonObject &o) {
        return QString::fromUtf8(QJsonDocument(o).toJson(QJsonDocument::Compact));
    }

    QJsonObject string(int maxLength, const QString &description) {
        return {{QStringLiteral("type"), QStringLiteral("string")},
                {QStringLiteral("maxLength"), maxLength},
                {QStringLiteral("description"), description}};
    }

    QJsonObject object(const QJsonObject &properties, const QStringList &required = {}) {
        QJsonObject o{{QStringLiteral("type"), QStringLiteral("object")},
                      {QStringLiteral("additionalProperties"), false},
                      {QStringLiteral("properties"), properties}};
        if (!required.isEmpty())
            o.insert(QStringLiteral("required"), QJsonArray::fromStringList(required));
        return o;
    }

    struct Hints {
        bool readOnly    = false;
        bool destructive = false;
        bool idempotent  = false;
        bool openWorld   = false;
        bool confirm     = false;
        bool untrusted   = false;
        QString notDestructiveBecause;
    };

    QJsonObject tool(const QString &appId, const QString &action, const QString &title,
                     const QString &description, const QJsonObject &input, const Hints &h,
                     const QJsonObject &output = {}) {
        QJsonObject meta{{kMeta + QStringLiteral("action"), action},
                         {kMeta + QStringLiteral("confirm"), h.confirm},
                         {kMeta + QStringLiteral("result"),
                          output.isEmpty() ? QStringLiteral("none") : QStringLiteral("object")},
                         {kMeta + QStringLiteral("sensitive"), false},
                         {kMeta + QStringLiteral("timeoutMs"), 25000}};
        if (h.untrusted)
            meta.insert(kMeta + QStringLiteral("untrusted"), true);
        if (!h.notDestructiveBecause.isEmpty())
            meta.insert(kMeta + QStringLiteral("notDestructiveBecause"), h.notDestructiveBecause);
        QJsonObject t{{QStringLiteral("_meta"), meta},
                      {QStringLiteral("annotations"),
                       QJsonObject{{QStringLiteral("destructiveHint"), h.destructive},
                                   {QStringLiteral("idempotentHint"), h.idempotent},
                                   {QStringLiteral("openWorldHint"), h.openWorld},
                                   {QStringLiteral("readOnlyHint"), h.readOnly}}},
                      {QStringLiteral("description"), description},
                      {QStringLiteral("inputSchema"), input},
                      {QStringLiteral("name"), appId + u'/' + action},
                      {QStringLiteral("title"), title}};
        if (!output.isEmpty())
            t.insert(QStringLiteral("outputSchema"), output);
        return t;
    }

    // Every parameter of these tools is a bounded string.
    QString invalid(const QJsonObject &schema, const QJsonObject &args) {
        const QJsonObject properties = schema.value(QLatin1String("properties")).toObject();
        for (auto it = args.constBegin(); it != args.constEnd(); ++it) {
            const QJsonObject property = properties.value(it.key()).toObject();
            if (property.isEmpty())
                return QStringLiteral("unknown argument ") + it.key().left(64);
            const int maxLength = property.value(QLatin1String("maxLength")).toInt();
            if (!it->isString() || it->toString().size() > maxLength)
                return it.key() + QStringLiteral(" must be a string of at most ") +
                    QString::number(maxLength) + QStringLiteral(" characters");
        }
        const QJsonArray required = schema.value(QLatin1String("required")).toArray();
        for (const auto &name : required) {
            if (args.value(name.toString()).toString().trimmed().isEmpty())
                return QStringLiteral("missing argument ") + name.toString();
        }
        return {};
    }

    bool runsOneOf(qint64 pid, const QStringList &paths) {
        struct stat exe {};
        const QByteArray link = "/proc/" + QByteArray::number(pid) + "/exe";
        if (pid <= 0 || ::stat(link.constData(), &exe) != 0)
            return false;
        for (const QString &path : paths) {
            struct stat candidate {};
            if (::stat(QFile::encodeName(path).constData(), &candidate) == 0 &&
                candidate.st_dev == exe.st_dev && candidate.st_ino == exe.st_ino)
                return true;
        }
        return false;
    }

    // Schemes that would reach files, other apps' data or code, not an app's links.
    bool schemeAllowed(const QString &scheme) {
        static const QStringList blocked{
            QStringLiteral("file"),  QStringLiteral("content"), QStringLiteral("javascript"),
            QStringLiteral("data"),  QStringLiteral("intent"),  QStringLiteral("android-app"),
            QStringLiteral("about"), QStringLiteral("jar"),     QStringLiteral("blob"),
            QStringLiteral("android.resource")};
        return !blocked.contains(scheme);
    }

    QString quoted(QString s) {
        s.replace(u'\\', QStringLiteral("\\\\")).replace(u'"', QStringLiteral("\\\""));
        s.replace(u'\n', QStringLiteral("\\n")).replace(u'\t', u' ');
        return u'"' + s + u'"';
    }

} // namespace

namespace KeelAndroid {

    QString appId(const QString &pkg) {
        return QStringLiteral("waydroid.") + pkg;
    }

    std::optional<App> app(const QJsonObject &m, QString *error) {
        App a;
        a.pkg = m.value(QStringLiteral("pkg")).toString();
        if (!Validate::isPackageName(a.pkg) || appId(a.pkg).size() > 255 ||
            a.pkg.startsWith(QLatin1String("org.marathonos.deckard."))) {
            *error = QStringLiteral("bad package name");
            return std::nullopt;
        }
        a.label = Validate::label(m, QStringLiteral("label"));
        if (a.label.isEmpty())
            a.label = a.pkg;

        static const QRegularExpression scheme(QStringLiteral("^[a-z][a-z0-9+.-]{0,31}$"));
        static const QRegularExpression host(QStringLiteral("^[a-z0-9*][a-z0-9.*-]{0,252}$"));
        QSet<QString> seen;
        for (const QJsonValue &v : m.value(QStringLiteral("links")).toArray()) {
            const QJsonObject o = v.toObject();
            Link l{o.value(QStringLiteral("s")).toString().toLower(),
                   o.value(QStringLiteral("h")).toString().toLower()};
            if (!scheme.match(l.scheme).hasMatch() || !schemeAllowed(l.scheme) ||
                (!l.host.isEmpty() && !host.match(l.host).hasMatch()))
                continue;
            const QString key = l.scheme + u':' + l.host;
            if (seen.contains(key) || a.links.size() >= 16)
                continue;
            seen.insert(key);
            a.links << l;
        }
        a.share = m.value(QStringLiteral("share")).toBool();
        const QString search = m.value(QStringLiteral("search")).toString();
        if (search == QLatin1String("gms") || search == QLatin1String("android"))
            a.search = search;

        a.shortcuts = shortcuts(m.value(QStringLiteral("shortcuts")).toArray());
        return a;
    }

    QList<Shortcut> shortcuts(const QJsonArray &list) {
        QList<Shortcut> out;
        QSet<QString>   ids;
        for (const QJsonValue &v : list) {
            const QJsonObject o  = v.toObject();
            const QString     id = o.value(QStringLiteral("id")).toString();
            Shortcut          s{id, Validate::text(o, QStringLiteral("label"), 64).simplified(),
                       Validate::text(o, QStringLiteral("long"), 256).simplified()};
            // The id goes back to Android as it came: it must be clean already.
            if (id.isEmpty() || id.size() > 128 || Validate::text(o, QStringLiteral("id"), 128) != id ||
                s.label.isEmpty() || ids.contains(id) || out.size() >= 15)
                continue;
            ids.insert(id);
            out << s;
        }
        return out;
    }

    QJsonObject toJson(const App &a) {
        QJsonArray links;
        for (const Link &l : a.links)
            links.append(QJsonObject{{QStringLiteral("s"), l.scheme}, {QStringLiteral("h"), l.host}});
        QJsonArray shortcuts;
        for (const Shortcut &s : a.shortcuts)
            shortcuts.append(QJsonObject{{QStringLiteral("id"), s.id},
                                         {QStringLiteral("label"), s.label},
                                         {QStringLiteral("long"), s.longLabel}});
        return {{QStringLiteral("pkg"), a.pkg},
                {QStringLiteral("label"), a.label},
                {QStringLiteral("links"), links},
                {QStringLiteral("share"), a.share},
                {QStringLiteral("search"), a.search},
                {QStringLiteral("shortcuts"), shortcuts}};
    }

    QString shortcutAction(const QString &id, const QStringList &taken) {
        QString word;
        for (const QChar c : id.toLower()) {
            if ((c >= u'a' && c <= u'z') || (c >= u'0' && c <= u'9'))
                word.append(c);
            else if (!word.isEmpty() && !word.endsWith(u'_'))
                word.append(u'_');
        }
        while (word.endsWith(u'_'))
            word.chop(1);
        if (word.isEmpty() || !word.front().isLetter())
            word.prepend(u's');
        word.truncate(40);
        while (word.endsWith(u'_'))
            word.chop(1);
        QString action = QStringLiteral("shortcut.") + word;
        for (int n = 2; taken.contains(action); ++n)
            action = QStringLiteral("shortcut.") + word + u'_' + QString::number(n);
        return action;
    }

    bool needsConfirmation(const QString &label) {
        static const QRegularExpression risky(
            QStringLiteral("\\b(delete|remove|erase|wipe|clear|reset|send|pay|payment|buy|order|"
                           "purchase|transfer|call|dial|post|publish|record|uninstall|block|report|"
                           "sign out|log out|logout)\\b"),
            QRegularExpression::CaseInsensitiveOption);
        return risky.match(label).hasMatch();
    }

    bool linkAllowed(const QString &uri, QString *error) {
        static const QRegularExpression shape(QStringLiteral("^([A-Za-z][A-Za-z0-9+.-]{0,31}):\\S+$"));
        const QRegularExpressionMatch m = shape.match(uri);
        if (uri.size() > 2048 || !m.hasMatch()) {
            *error = QStringLiteral("link must be a URI of at most 2048 characters");
            return false;
        }
        for (const QChar c : uri) {
            if (c.category() == QChar::Other_Control || c.category() == QChar::Other_Format) {
                *error = QStringLiteral("link has control characters");
                return false;
            }
        }
        const QString scheme = m.captured(1).toLower();
        if (!schemeAllowed(scheme)) {
            *error = scheme + QStringLiteral(": links are not opened in apps");
            return false;
        }
        if ((scheme == QLatin1String("http") || scheme == QLatin1String("https")) &&
            QUrl(uri, QUrl::StrictMode).host().isEmpty()) {
            *error = QStringLiteral("a web link needs a host");
            return false;
        }
        return true;
    }

    Manifest manifest(const App &a) {
        const QString id = appId(a.pkg);
        QList<QJsonObject> tools;

        QStringList examples;
        for (const Link &l : a.links) {
            if (examples.size() < 8)
                examples << (l.host.isEmpty() ? l.scheme + u':' : l.scheme + QStringLiteral("://") + l.host);
        }
        QJsonObject show;
        QString     showText = QStringLiteral("Open %1, an Android app, and bring it to the front.").arg(a.label);
        if (!examples.isEmpty()) {
            show.insert(QStringLiteral("link"),
                        string(2048, QStringLiteral("A link or deep link for the app to open")));
            showText += QStringLiteral(" Given a link, opens it in the app; it handles links like %1.")
                            .arg(examples.join(QStringLiteral(", ")));
        }
        Hints showHints;
        showHints.idempotent            = true;
        showHints.notDestructiveBecause = QStringLiteral("it only shows the app, or a link in it");
        tools << tool(id, QStringLiteral("app.show"), QStringLiteral("Open ") + a.label, showText,
                      object(show), showHints);

        if (a.share) {
            Hints h;
            h.openWorld             = true;
            h.notDestructiveBecause = QStringLiteral("it opens the app's share screen; the person decides whether to send");
            tools << tool(id, QStringLiteral("share.text"), QStringLiteral("Share to ") + a.label,
                          QStringLiteral("Open %1's share screen with the text (and an optional "
                                         "subject) filled in. Nothing leaves the app until the "
                                         "person goes on from there.")
                              .arg(a.label),
                          object({{QStringLiteral("text"), string(20000, QStringLiteral("The text to share"))},
                                  {QStringLiteral("subject"), string(200, QStringLiteral("A subject, for apps that use one"))}},
                                 {QStringLiteral("text")}),
                          h);
        }
        if (!a.search.isEmpty()) {
            Hints h;
            h.idempotent = true;
            tools << tool(id, QStringLiteral("app.search"), QStringLiteral("Search in ") + a.label,
                          QStringLiteral("Search inside %1 and show its results for the query.").arg(a.label),
                          object({{QStringLiteral("query"), string(500, QStringLiteral("What to search for"))}},
                                 {QStringLiteral("query")}),
                          h);
        }

        Manifest    out;
        QStringList taken;
        for (const Shortcut &s : a.shortcuts) {
            const QString action = shortcutAction(s.id, taken);
            taken << action;
            const bool risky = needsConfirmation(s.label + u' ' + s.longLabel);
            Hints      h;
            h.confirm     = risky;
            h.destructive = risky;
            if (!risky)
                h.notDestructiveBecause = QStringLiteral("it opens a screen of the app");
            const QString what = s.longLabel.isEmpty() ? s.label : s.longLabel;
            tools << tool(id, action, QStringLiteral("%1 (%2)").arg(s.label, a.label),
                          QStringLiteral("%1: a shortcut of %2, an Android app; opens the app there.%3")
                              .arg(what, a.label,
                                   risky ? QStringLiteral(" It may act at once, so the person confirms it.")
                                         : QString()),
                          object({}), h);
            out.shortcuts.insert(action, s.id);
        }

        Hints read;
        read.readOnly   = true;
        read.idempotent = true;
        read.untrusted  = true;
        tools << tool(
            id, QStringLiteral("screen.read"), QStringLiteral("Read %1's screen").arg(a.label),
            QStringLiteral("Read what %1 shows on screen now, as text: one line per element with its id "
                           "([snapshot.node]), role, text, description, hint and state. Password fields "
                           "are never included. Only while the app is in front and the phone is unlocked; "
                           "ui.tap and ui.type take the ids.")
                .arg(a.label),
            object({}), read,
            object({{QStringLiteral("app"), QJsonObject{{QStringLiteral("type"), QStringLiteral("string")},
                                                         {QStringLiteral("maxLength"), 264}}},
                    {QStringLiteral("label"), QJsonObject{{QStringLiteral("type"), QStringLiteral("string")},
                                                           {QStringLiteral("maxLength"), 128}}},
                    {QStringLiteral("title"), QJsonObject{{QStringLiteral("type"), QStringLiteral("string")},
                                                           {QStringLiteral("maxLength"), 256}}},
                    {QStringLiteral("snapshot"), QJsonObject{{QStringLiteral("type"), QStringLiteral("integer")},
                                                              {QStringLiteral("minimum"), 1}}},
                    {QStringLiteral("screen"), QJsonObject{{QStringLiteral("type"), QStringLiteral("string")},
                                                            {QStringLiteral("maxLength"), 20000}}},
                    {QStringLiteral("truncated"), QJsonObject{{QStringLiteral("type"), QStringLiteral("boolean")}}}},
                   {QStringLiteral("app"), QStringLiteral("snapshot"), QStringLiteral("screen")}));

        Hints act;
        act.confirm     = true;
        act.destructive = true;
        const QJsonObject node =
            string(24, QStringLiteral("An element id from the latest screen.read, such as 3.17"));
        tools << tool(id, QStringLiteral("ui.tap"), QStringLiteral("Tap in ") + a.label,
                      QStringLiteral("Tap an element of %1's screen, by its id from screen.read, as the "
                                     "person would. The person confirms every tap.")
                          .arg(a.label),
                      object({{QStringLiteral("node"), node}}, {QStringLiteral("node")}), act);
        tools << tool(id, QStringLiteral("ui.type"), QStringLiteral("Type in ") + a.label,
                      QStringLiteral("Replace the text of an editable field of %1's screen (an id from "
                                     "screen.read) with the given text, as the person would type it. "
                                     "Never for password fields. The person confirms it.")
                          .arg(a.label),
                      object({{QStringLiteral("node"), node},
                              {QStringLiteral("text"), string(4000, QStringLiteral("The field's new text"))}},
                             {QStringLiteral("node"), QStringLiteral("text")}),
                      act);

        QJsonArray list;
        for (const QJsonObject &t : std::as_const(tools)) {
            // keel-mcp drops a tool whose MCP name (with __) is over 128 characters.
            if (t.value(QStringLiteral("name")).toString().size() + 1 <= 128)
                list.append(t);
        }
        out.json = {{QStringLiteral("appId"), id},
                    {QStringLiteral("context"),
                     QJsonObject{{QStringLiteral("uri"), QStringLiteral("keel://") + id + QStringLiteral("/context")}}},
                    {QStringLiteral("entities"), QJsonArray()},
                    {QStringLiteral("prompts"), QJsonArray()},
                    {QStringLiteral("tools"), list},
                    {QStringLiteral("version"), 1}};
        return out;
    }

    std::optional<Screen> screen(const QJsonObject &m, QString *error) {
        Screen s;
        s.pkg = m.value(QStringLiteral("pkg")).toString();
        const double snap = m.value(QStringLiteral("snap")).toDouble();
        if (!Validate::isPackageName(s.pkg) || snap < 1 || snap > 2147483647.0 || snap != qint64(snap)) {
            *error = QStringLiteral("bad screen header");
            return std::nullopt;
        }
        s.snapshot = qint64(snap);
        s.title    = Validate::text(m, QStringLiteral("title"), 256).simplified();
        const QJsonArray nodes = m.value(QStringLiteral("nodes")).toArray();
        for (const QJsonValue &v : nodes) {
            if (s.nodes.size() >= 400)
                break;
            const QJsonObject o = v.toObject();
            Node              n;
            n.index  = qBound(0, o.value(QStringLiteral("i")).toInt(-1), 9999);
            n.depth  = qBound(0, o.value(QStringLiteral("d")).toInt(), 63);
            n.cls    = Validate::text(o, QStringLiteral("c"), 128);
            n.text   = Validate::text(o, QStringLiteral("t"), 1000);
            n.desc   = Validate::text(o, QStringLiteral("cd"), 500);
            n.hint   = Validate::text(o, QStringLiteral("h"), 200);
            n.viewId = Validate::text(o, QStringLiteral("v"), 128);
            n.flags  = o.value(QStringLiteral("f")).toInt() & AllFlags;
            if (o.value(QStringLiteral("i")).toInt(-1) < 0)
                continue;
            if (n.flags & Password)
                n.text.clear();
            s.nodes << n;
        }
        return s;
    }

    QString role(const Node &n) {
        const QString c = n.cls.section(u'.', -1);
        const auto    has = [&c](const char *what) { return c.contains(QLatin1String(what)); };
        if ((n.flags & Editable) || has("EditText"))
            return QStringLiteral("field");
        if (has("CheckBox"))
            return QStringLiteral("checkbox");
        if (has("Switch") || has("ToggleButton"))
            return QStringLiteral("switch");
        if (has("RadioButton"))
            return QStringLiteral("radio");
        if (has("Button"))
            return QStringLiteral("button");
        if (has("ImageView"))
            return QStringLiteral("image");
        if (has("WebView"))
            return QStringLiteral("web");
        if (has("SeekBar") || has("Slider"))
            return QStringLiteral("slider");
        if (has("ProgressBar"))
            return QStringLiteral("progress");
        if (has("RecyclerView") || has("ListView") || has("GridView") || has("ScrollView") ||
            has("ViewPager"))
            return QStringLiteral("list");
        if (has("TextView"))
            return QStringLiteral("text");
        if (n.flags & Clickable)
            return QStringLiteral("item");
        return QStringLiteral("group");
    }

    QString render(const Screen &s, int maxChars, bool *truncated) {
        static const QStringList tappable{QStringLiteral("button"), QStringLiteral("field"),
                                          QStringLiteral("checkbox"), QStringLiteral("switch"),
                                          QStringLiteral("radio"), QStringLiteral("item")};
        QString out;
        *truncated = false;
        for (const Node &n : s.nodes) {
            const QString r    = role(n);
            QString       line = QString(2 * qMin(n.depth, 16), u' ') + u'[' + QString::number(s.snapshot) +
                u'.' + QString::number(n.index) + QStringLiteral("] ") + r;
            if (n.flags & Password)
                line += QStringLiteral(" password (hidden)");
            else if (!n.text.isEmpty())
                line += u' ' + quoted(n.text);
            if (!n.desc.isEmpty() && n.desc != n.text)
                line += QStringLiteral(" (") + n.desc.simplified() + u')';
            if (!n.hint.isEmpty() && n.hint != n.text)
                line += QStringLiteral(" hint ") + quoted(n.hint);
            const QString view = n.viewId.section(QStringLiteral(":id/"), -1);
            if (!view.isEmpty())
                line += QStringLiteral(" #") + view;
            QStringList state;
            if (n.flags & Checkable)
                state << ((n.flags & Checked) ? QStringLiteral("checked") : QStringLiteral("unchecked"));
            if ((n.flags & Clickable) && !tappable.contains(r))
                state << QStringLiteral("tappable");
            if (n.flags & LongClickable)
                state << QStringLiteral("long-press");
            if (n.flags & Selected)
                state << QStringLiteral("selected");
            if (n.flags & Focused)
                state << QStringLiteral("focused");
            if (n.flags & Scrollable)
                state << QStringLiteral("scrolls");
            if (n.flags & Heading)
                state << QStringLiteral("heading");
            if (n.flags & Disabled)
                state << QStringLiteral("disabled");
            if (!state.isEmpty())
                line += QStringLiteral(" {") + state.join(QStringLiteral(", ")) + u'}';
            if (out.size() + line.size() + 1 > maxChars) {
                *truncated = true;
                break;
            }
            out += line + u'\n';
        }
        return out;
    }

    bool parseNode(const QString &id, qint64 *snapshot, int *index) {
        static const QRegularExpression shape(QStringLiteral("^([1-9][0-9]{0,9})\\.([0-9]{1,4})$"));
        const QRegularExpressionMatch m = shape.match(id.trimmed());
        if (!m.hasMatch())
            return false;
        *snapshot = m.captured(1).toLongLong();
        *index    = m.captured(2).toInt();
        return *snapshot <= 2147483647;
    }

    bool isExternalHost(const QByteArray &cmdline) {
        const QList<QByteArray> args = cmdline.split('\0');
        return args.contains(QByteArrayLiteral("--external"));
    }

} // namespace KeelAndroid

using namespace KeelAndroid;

KeelProvider::KeelProvider(KeelApps *apps, const QString &pkg, QObject *parent)
    : QObject(parent)
    , m_apps(apps)
    , m_pkg(pkg) {
}

QString KeelProvider::Describe() {
    return compact(m_apps->manifest(m_pkg));
}

QString KeelProvider::Invoke(const QString &action, const QString &arguments,
                             const QVariantMap &options) {
    bool external = false;
    if (!m_apps->trustedCaller(message(), connection(), &external)) {
        sendErrorReply(QDBusError::AccessDenied,
                       QStringLiteral("only keel-mcp and Pilot may use an Android app's actions"));
        return {};
    }
    const QJsonDocument doc = QJsonDocument::fromJson(arguments.toUtf8());
    if (!doc.isObject()) {
        sendErrorReply(kErrors + QStringLiteral("InvalidArguments"),
                       QStringLiteral("arguments must be a JSON object"));
        return {};
    }
    setDelayedReply(true);
    m_apps->invoke(m_pkg, action, doc.object(), options.value(QStringLiteral("userInitiated")).toBool(),
                   external, message(), connection());
    return {};
}

QString KeelProvider::GetEntity(const QString &type, const QString &) {
    sendErrorReply(kErrors + QStringLiteral("UnknownAction"), QStringLiteral("no entity type ") + type.left(64));
    return {};
}

QString KeelProvider::FindEntities(const QString &type, const QString &, uint) {
    sendErrorReply(kErrors + QStringLiteral("UnknownAction"), QStringLiteral("no entity type ") + type.left(64));
    return {};
}

QString KeelProvider::GetContext() {
    bool external = false;
    if (!m_apps->trustedCaller(message(), connection(), &external)) {
        sendErrorReply(QDBusError::AccessDenied,
                       QStringLiteral("only keel-mcp and Pilot may read an Android app's context"));
        return {};
    }
    setDelayedReply(true);
    m_apps->context(m_pkg, external, message(), connection());
    return {};
}

KeelApps::KeelApps(Send send, Send system, Connect connect, QDBusConnection shell, QStringList trusted,
                   const QString &statePath, QObject *parent)
    : QObject(parent)
    , m_send(std::move(send))
    , m_system(std::move(system))
    , m_connect(std::move(connect))
    , m_shell(std::move(shell))
    , m_trusted(std::move(trusted))
    , m_statePath(statePath) {
    load();
    m_shell.connect(QStringLiteral("org.marathonos.Shell.Session"), QStringLiteral("/org/marathonos/Shell/Navigation"),
                    QStringLiteral("org.marathonos.Shell.Navigation1"), QStringLiteral("ForegroundAppChanged"), this,
                    SLOT(onForeground(QString)));
}

KeelApps::~KeelApps() {
    const QStringList pkgs = m_apps.keys();
    for (const QString &pkg : pkgs)
        unpublish(pkg);
}

bool KeelApps::handles(const QString &type) {
    static const QStringList types{QStringLiteral("app_info"), QStringLiteral("app_gone"),
                                   QStringLiteral("app_list"), QStringLiteral("launched"),
                                   QStringLiteral("screen"),   QStringLiteral("ui_done")};
    return types.contains(type);
}

void KeelApps::handle(const QString &type, const QJsonObject &message) {
    if (type == QLatin1String("app_info"))
        onApp(message);
    else if (type == QLatin1String("app_gone"))
        onAppGone(message.value(QStringLiteral("pkg")).toString());
    else if (type == QLatin1String("app_list"))
        onAppList(message);
    else
        onReply(message);
}

bool KeelApps::handlesSystem(const QString &type) {
    return type == QLatin1String("keel_done") || type == QLatin1String("keel_changed");
}

void KeelApps::handleSystem(const QString &type, const QJsonObject &message) {
    if (type == QLatin1String("keel_changed")) {
        const QString pkg = message.value(QStringLiteral("pkg")).toString();
        if (m_apps.contains(pkg))
            refreshShortcuts(pkg);
        return;
    }
    const qint64 id = qint64(message.value(QStringLiteral("req")).toDouble());
    auto         it = m_requests.find(id);
    if (it == m_requests.end() || !it->sent || !it->system)
        return;
    Request r = m_requests.take(id);
    delete r.timer;
    r.done(true, message);
}

void KeelApps::setSystemConnected(bool connected) {
    connectedChanged(true, connected);
    if (connected) {
        for (const QString &pkg : m_apps.keys())
            refreshShortcuts(pkg);
    }
}

// Launcher shortcuts come from DeckardSystem, which as the system uid may
// read and start every app's: DeckardBridge may not.
void KeelApps::refreshShortcuts(const QString &pkg) {
    if (!m_systemConnected)
        return;
    request({{QStringLiteral("t"), QStringLiteral("keel")},
             {QStringLiteral("op"), QStringLiteral("shortcuts")},
             {QStringLiteral("pkg"), pkg}},
            [this, pkg](bool ok, const QJsonObject &reply) {
                auto it = m_apps.find(pkg);
                if (!ok || !reply.value(QStringLiteral("ok")).toBool() || it == m_apps.end())
                    return;
                App a       = it->app;
                a.shortcuts = shortcuts(reply.value(QStringLiteral("shortcuts")).toArray());
                publish(a);
                save();
            },
            true);
}

void KeelApps::notSecure(const QString &pkg, Done done) {
    if (!m_systemConnected)
        return done(QStringLiteral("NotAvailable"),
                    QStringLiteral("Android cannot say whether the screen is protected"));
    const QString label = m_apps.value(pkg).app.label;
    request({{QStringLiteral("t"), QStringLiteral("keel")},
             {QStringLiteral("op"), QStringLiteral("secure")},
             {QStringLiteral("pkg"), pkg}},
            [this, pkg, label, done = std::move(done)](bool ok, const QJsonObject &reply) {
                if (!ok || !reply.value(QStringLiteral("ok")).toBool())
                    return done(QStringLiteral("NotAvailable"),
                                QStringLiteral("Android cannot say whether the screen is protected"));
                const bool secure = reply.value(QStringLiteral("secure")).toBool(true);
                reportSecure(pkg, secure);
                if (secure)
                    return done(QStringLiteral("NotAvailable"),
                                label + QStringLiteral(" protects this screen from being read"));
                done({}, {});
            },
            true);
}

// The shell keeps a secure app's windows off its card and out of
// screenshots; deckardd tells it when an Android app comes to the front.
void KeelApps::onForeground(const QString &id) {
    if (id.startsWith(QLatin1String("waydroid.")) && m_apps.contains(id.mid(9)))
        checkSecure(id.mid(9), true);
}

void KeelApps::checkSecure(const QString &pkg, bool retry) {
    if (!m_systemConnected)
        return;
    request({{QStringLiteral("t"), QStringLiteral("keel")},
             {QStringLiteral("op"), QStringLiteral("secure")},
             {QStringLiteral("pkg"), pkg}},
            [this, pkg, retry](bool ok, const QJsonObject &reply) {
                if (ok && reply.value(QStringLiteral("ok")).toBool())
                    return reportSecure(pkg, reply.value(QStringLiteral("secure")).toBool());
                // A window that is still starting is not in the dump yet.
                if (retry)
                    QTimer::singleShot(2000, this, [this, pkg] { checkSecure(pkg, false); });
            },
            true);
}

void KeelApps::reportSecure(const QString &pkg, bool secure) {
    if (m_secure.value(pkg, false) == secure)
        return;
    m_secure.insert(pkg, secure);
    QDBusMessage call = QDBusMessage::createMethodCall(
        QStringLiteral("org.marathonos.Shell.Session"), QStringLiteral("/org/marathonos/Shell/Navigation"),
        QStringLiteral("org.marathonos.Shell.Navigation1"), QStringLiteral("SetAppWindowSecure"));
    call << appId(pkg) << secure;
    m_shell.call(call, QDBus::NoBlock);
    qCInfo(lcKeel) << appId(pkg) << (secure ? "is secure" : "is not secure");
}

QStringList KeelApps::packages() const {
    QStringList out = m_apps.keys();
    out.sort();
    return out;
}

QJsonObject KeelApps::manifest(const QString &pkg) const {
    return m_apps.value(pkg).manifest.json;
}

bool KeelApps::trustedCaller(const QDBusMessage &call, QDBusConnection connection, bool *external) const {
    const QDBusReply<uint> pid = connection.interface()->servicePid(call.service());
    if (!pid.isValid() || !runsOneOf(pid.value(), m_trusted))
        return false;
    QFile cmdline(QStringLiteral("/proc/%1/cmdline").arg(pid.value()));
    *external = cmdline.open(QIODevice::ReadOnly) && isExternalHost(cmdline.read(65536));
    return true;
}

void KeelApps::onApp(const QJsonObject &message) {
    QString    error;
    const auto a = app(message, &error);
    if (!a) {
        qCWarning(lcKeel) << "app_info refused:" << error;
        return;
    }
    App merged = *a;
    if (const auto it = m_apps.constFind(merged.pkg); it != m_apps.cend())
        merged.shortcuts = it->app.shortcuts;
    publish(merged);
    save();
    refreshShortcuts(merged.pkg);
}

void KeelApps::onAppGone(const QString &pkg) {
    if (!m_apps.contains(pkg))
        return;
    unpublish(pkg);
    save();
}

void KeelApps::onAppList(const QJsonObject &message) {
    QSet<QString> listed;
    for (const QJsonValue &v : message.value(QStringLiteral("pkgs")).toArray())
        listed.insert(v.toString());
    bool changed = false;
    for (const QString &pkg : m_apps.keys()) {
        if (!listed.contains(pkg)) {
            unpublish(pkg);
            changed = true;
        }
    }
    if (changed)
        save();
}

void KeelApps::publish(const App &a) {
    Manifest m  = KeelAndroid::manifest(a);
    auto     it = m_apps.find(a.pkg);
    if (it != m_apps.end() && it->manifest.json == m.json) {
        it->app = a;
        return;
    }
    // A new connection for a changed manifest: keel-mcp describes a name
    // again only when its owner changes.
    if (it != m_apps.end())
        unpublish(a.pkg);
    if (m_apps.size() >= kMaxApps) {
        qCWarning(lcKeel) << "too many apps; not publishing" << a.pkg;
        return;
    }
    Entry e;
    e.app        = a;
    e.manifest   = std::move(m);
    e.connection = QStringLiteral("deckard-keel-%1").arg(++m_connections);
    QDBusConnection bus      = m_connect(e.connection);
    auto           *provider = new KeelProvider(this, a.pkg, this);
    if (!bus.isConnected() || !bus.registerObject(kPath, provider, QDBusConnection::ExportAllSlots) ||
        !bus.registerService(appId(a.pkg))) {
        qCWarning(lcKeel) << "cannot publish" << appId(a.pkg) << bus.lastError().message();
        delete provider;
        QDBusConnection::disconnectFromBus(e.connection);
        return;
    }
    e.provider = provider;
    m_apps.insert(a.pkg, e);
    qCInfo(lcKeel) << "published" << appId(a.pkg) << "with"
                   << e.manifest.json.value(QStringLiteral("tools")).toArray().size() << "actions";
}

void KeelApps::unpublish(const QString &pkg) {
    const Entry e = m_apps.take(pkg);
    // Released before the next owner asks for the name.
    QDBusConnection(e.connection).unregisterService(appId(pkg));
    QDBusConnection::disconnectFromBus(e.connection);
    if (e.provider)
        e.provider->deleteLater();
    qCInfo(lcKeel) << "withdrew" << appId(pkg);
}

void KeelApps::save() const {
    if (m_statePath.isEmpty())
        return;
    QJsonArray list;
    for (const Entry &e : std::as_const(m_apps))
        list.append(toJson(e.app));
    QDir().mkpath(QFileInfo(m_statePath).absolutePath());
    QSaveFile file(m_statePath);
    if (file.open(QIODevice::WriteOnly)) {
        file.write(QJsonDocument(list).toJson(QJsonDocument::Compact));
        file.commit();
    }
}

void KeelApps::load() {
    QFile file(m_statePath);
    if (m_statePath.isEmpty() || !file.open(QIODevice::ReadOnly))
        return;
    const QJsonArray list = QJsonDocument::fromJson(file.read(4 << 20)).array();
    for (const QJsonValue &v : list) {
        QString error;
        if (const auto a = app(v.toObject(), &error))
            publish(*a);
    }
}

void KeelApps::setConnected(bool connected) {
    connectedChanged(false, connected);
}

void KeelApps::connectedChanged(bool system, bool connected) {
    (system ? m_systemConnected : m_connected) = connected;
    const QList<qint64> ids = m_requests.keys();
    for (const qint64 id : ids) {
        auto it = m_requests.find(id);
        if (it == m_requests.end() || it->system != system)
            continue;
        if (connected && !it->sent) {
            it->sent = true;
            (system ? m_system : m_send)(it->message);
        } else if (!connected && it->sent) {
            Request r = m_requests.take(id);
            delete r.timer;
            r.done(false, {{QStringLiteral("error"), QStringLiteral("Android went away")}});
        }
    }
}

void KeelApps::request(QJsonObject message, std::function<void(bool, const QJsonObject &)> done,
                       bool system) {
    const qint64 id = m_nextRequest;
    m_nextRequest   = m_nextRequest >= 2147483647 ? 1 : m_nextRequest + 1;
    message.insert(QStringLiteral("req"), double(id));
    Request r;
    r.message = message;
    r.system  = system;
    r.done    = std::move(done);
    r.timer   = new QTimer(this);
    r.timer->setSingleShot(true);
    connect(r.timer, &QTimer::timeout, this, [this, id, system] {
        Request timedOut = m_requests.take(id);
        timedOut.timer->deleteLater();
        const bool up = system ? m_systemConnected : m_connected;
        timedOut.done(false, {{QStringLiteral("error"), up ? QStringLiteral("Android did not answer")
                                                           : QStringLiteral("Android is not running")}});
    });
    r.timer->start(kBridgeTimeoutMs);
    if (system ? m_systemConnected : m_connected) {
        r.sent = true;
        (system ? m_system : m_send)(message);
    }
    m_requests.insert(id, r);
}

void KeelApps::onReply(const QJsonObject &message) {
    const qint64 id = qint64(message.value(QStringLiteral("req")).toDouble());
    auto         it = m_requests.find(id);
    if (it == m_requests.end() || !it->sent || it->system)
        return;
    Request r = m_requests.take(id);
    delete r.timer;
    r.done(true, message);
}

void KeelApps::shell(const QString &action, const QJsonObject &args, ShellDone done) {
    QDBusMessage call = QDBusMessage::createMethodCall(QStringLiteral("org.marathonos.shell"), kPath,
                                                       kInterface, QStringLiteral("Invoke"));
    call << action << compact(args) << QVariantMap();
    auto *watcher = new QDBusPendingCallWatcher(m_shell.asyncCall(call, 10000), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this,
            [done = std::move(done)](QDBusPendingCallWatcher *w) {
                w->deleteLater();
                const QDBusMessage reply = w->reply();
                if (reply.type() != QDBusMessage::ReplyMessage || reply.arguments().isEmpty()) {
                    const QString name = reply.errorName();
                    done(false, {}, name.startsWith(kErrors) ? name.mid(kErrors.size()) : QStringLiteral("Failed"),
                         reply.errorMessage().isEmpty() ? QStringLiteral("the shell did not answer")
                                                        : reply.errorMessage());
                    return;
                }
                done(true, QJsonDocument::fromJson(reply.arguments().constFirst().toString().toUtf8()).object(),
                     {}, {});
            });
}

void KeelApps::unlocked(const QString &pkg, bool inFront, Done done) {
    const QString id    = appId(pkg);
    const QString label = m_apps.value(pkg).app.label;
    shell(QStringLiteral("shell.state"), {},
          [id, label, inFront, done = std::move(done)](bool ok, const QJsonObject &state, const QString &code,
                                                      const QString &message) {
              if (!ok)
                  return done(code, message);
              if (state.value(QStringLiteral("locked")).toBool())
                  return done(QStringLiteral("NotAvailable"), QStringLiteral("the phone is locked"));
              if (inFront && (!state.value(QStringLiteral("screenOn")).toBool() ||
                              state.value(QStringLiteral("foregroundApp")).toString() != id))
                  return done(QStringLiteral("NotAvailable"),
                              label + QStringLiteral(" is not on screen: open it first with app.show"));
              done({}, {});
          });
}

void KeelApps::waitForWindow(const QString &id, int left, Done done) {
    shell(QStringLiteral("apps.running"), {},
          [this, id, left, done = std::move(done)](bool ok, const QJsonObject &result, const QString &code,
                                                  const QString &message) mutable {
              if (!ok)
                  return done(code, message);
              for (const QJsonValue &v : result.value(QStringLiteral("result")).toArray()) {
                  if (v.toObject().value(QStringLiteral("id")).toString() == id)
                      return done({}, {});
              }
              if (left <= 0)
                  return done(QStringLiteral("Failed"), QStringLiteral("the app's window did not appear"));
              QTimer::singleShot(250, this, [this, id, left, done = std::move(done)]() mutable {
                  waitForWindow(id, left - 1, std::move(done));
              });
          });
}

// The shell's Navigation1.ExpectAppWindow(appId) (proposed): it shows the
// app's window now and lets a new window of the app take the front for a
// while, as for a launch from Marathon.
void KeelApps::expectWindow(const QString &id, std::function<void(bool)> done) {
    QDBusMessage call = QDBusMessage::createMethodCall(
        QStringLiteral("org.marathonos.Shell.Session"), QStringLiteral("/org/marathonos/Shell/Navigation"),
        QStringLiteral("org.marathonos.Shell.Navigation1"), QStringLiteral("ExpectAppWindow"));
    call << id;
    auto *watcher = new QDBusPendingCallWatcher(m_shell.asyncCall(call, 5000), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this,
            [done = std::move(done)](QDBusPendingCallWatcher *w) {
                w->deleteLater();
                const QDBusMessage reply = w->reply();
                done(reply.type() == QDBusMessage::ReplyMessage && !reply.arguments().isEmpty() &&
                     reply.arguments().constFirst().toBool());
            });
}

// Android starts the activity, in the app's task or a new one. With a shell
// that expects the window it comes forward as a launch; otherwise deckardd
// waits for the shell to file it and asks app.launch to show it. A launch
// through the shell first would race Android's launcher intent against
// this one.
void KeelApps::launchThenShow(const QString &pkg, const QJsonObject &launch, bool system, Done done) {
    const QString id   = appId(pkg);
    const auto    show = [this, id](const Done &then) {
        waitForWindow(id, kWindowWaitMs / 250, [this, id, then](const QString &code, const QString &message) {
            if (!code.isEmpty())
                return then(code, message);
            shell(QStringLiteral("app.launch"), {{QStringLiteral("id"), id}},
                  [then](bool ok, const QJsonObject &, const QString &code, const QString &message) {
                      ok ? then({}, {}) : then(code, message);
                  });
        });
    };
    const auto launched = [](const Done &then) {
        return [then](bool ok, const QJsonObject &reply) {
            if (!ok)
                return then(QStringLiteral("NotAvailable"), reply.value(QStringLiteral("error")).toString());
            if (!reply.value(QStringLiteral("ok")).toBool())
                return then(QStringLiteral("Failed"), Validate::text(reply, QStringLiteral("error"), 256));
            then({}, {});
        };
    };
    if (system ? m_systemConnected : m_connected) {
        expectWindow(id, [this, launch, system, show, launched, done](bool expected) {
            if (expected)
                return request(launch, launched(done), system);
            request(launch, launched([show, done](const QString &code, const QString &message) {
                        code.isEmpty() ? show(done) : done(code, message);
                    }),
                    system);
        });
        return;
    }
    // Android is not running: the shell's launch starts it.
    shell(QStringLiteral("app.launch"), {{QStringLiteral("id"), id}},
          [this, launch, system, launched, done](bool ok, const QJsonObject &, const QString &code,
                                                 const QString &message) {
              if (!ok)
                  return done(code, message);
              request(launch, launched(done), system);
          });
}

void KeelApps::readScreen(const QString &pkg, int maxChars,
                          std::function<void(const QString &, const QString &, const QJsonObject &)> done) {
    request({{QStringLiteral("t"), QStringLiteral("screen_read")}, {QStringLiteral("pkg"), pkg}},
            [this, pkg, maxChars, done = std::move(done)](bool ok, const QJsonObject &reply) {
                if (!ok)
                    return done(QStringLiteral("NotAvailable"), reply.value(QStringLiteral("error")).toString(), {});
                if (!reply.value(QStringLiteral("ok")).toBool()) {
                    const QString why = Validate::text(reply, QStringLiteral("error"), 256);
                    return done(QStringLiteral("NotAvailable"),
                                why.isEmpty() ? QStringLiteral("the screen cannot be read") : why, {});
                }
                QString    error;
                const auto s = screen(reply, &error);
                auto       it = m_apps.find(pkg);
                if (!s || s->pkg != pkg || it == m_apps.end())
                    return done(QStringLiteral("Failed"), QStringLiteral("Android sent an unusable screen"), {});
                it->snapshot   = s->snapshot;
                bool truncated = false;
                const QString text = render(*s, maxChars, &truncated);
                done({}, {},
                     {{QStringLiteral("app"), appId(pkg)},
                      {QStringLiteral("label"), it->app.label},
                      {QStringLiteral("title"), s->title},
                      {QStringLiteral("snapshot"), double(s->snapshot)},
                      {QStringLiteral("screen"), text},
                      {QStringLiteral("truncated"), truncated}});
            });
}

void KeelApps::invoke(const QString &pkg, const QString &action, const QJsonObject &args,
                      bool userInitiated, bool external, const QDBusMessage &call,
                      QDBusConnection connection) {
    const auto fail = [call, connection](const QString &code, const QString &message) {
        connection.send(call.createErrorReply(kErrors + code, message));
    };
    const auto reply = [call, connection](const QJsonObject &result) {
        connection.send(call.createReply(compact(result)));
    };
    const Done finish = [fail, reply](const QString &code, const QString &message) {
        code.isEmpty() ? reply({}) : fail(code, message);
    };

    auto entry = m_apps.constFind(pkg);
    if (entry == m_apps.cend())
        return fail(QStringLiteral("NotAvailable"), QStringLiteral("the app is not installed"));
    QJsonObject tool;
    for (const QJsonValue &t : entry->manifest.json.value(QStringLiteral("tools")).toArray()) {
        if (t[QStringLiteral("_meta")][kMeta + QStringLiteral("action")].toString() == action)
            tool = t.toObject();
    }
    if (tool.isEmpty())
        return fail(QStringLiteral("UnknownAction"), QStringLiteral("no action named ") + action.left(64));
    if (const QString why = invalid(tool.value(QStringLiteral("inputSchema")).toObject(), args); !why.isEmpty())
        return fail(QStringLiteral("InvalidArguments"), why);
    qCInfo(lcKeel) << appId(pkg) << action;

    const auto arg = [&args](const char *key) { return args.value(QLatin1String(key)).toString(); };
    QJsonObject launch{{QStringLiteral("t"), QStringLiteral("launch")}, {QStringLiteral("pkg"), pkg}};

    if (action == QLatin1String("app.show")) {
        const QString link = arg("link").trimmed();
        if (link.isEmpty()) {
            shell(QStringLiteral("app.launch"), {{QStringLiteral("id"), appId(pkg)}},
                  [finish](bool ok, const QJsonObject &, const QString &code, const QString &message) {
                      ok ? finish({}, {}) : finish(code, message);
                  });
            return;
        }
        QString why;
        if (!linkAllowed(link, &why))
            return fail(QStringLiteral("InvalidArguments"), why);
        launch.insert(QStringLiteral("kind"), QStringLiteral("view"));
        launch.insert(QStringLiteral("uri"), link);
    } else if (action == QLatin1String("share.text")) {
        launch.insert(QStringLiteral("kind"), QStringLiteral("send"));
        launch.insert(QStringLiteral("text"), arg("text"));
        if (!arg("subject").isEmpty())
            launch.insert(QStringLiteral("subject"), arg("subject"));
    } else if (action == QLatin1String("app.search")) {
        launch.insert(QStringLiteral("kind"), QStringLiteral("search"));
        launch.insert(QStringLiteral("query"), arg("query").trimmed());
        launch.insert(QStringLiteral("mode"), entry->app.search);
    } else if (entry->manifest.shortcuts.contains(action)) {
        const QJsonObject start{{QStringLiteral("t"), QStringLiteral("keel")},
                                {QStringLiteral("op"), QStringLiteral("start")},
                                {QStringLiteral("pkg"), pkg},
                                {QStringLiteral("id"), entry->manifest.shortcuts.value(action)}};
        unlocked(pkg, false, [this, pkg, start, finish](const QString &code, const QString &message) {
            if (!code.isEmpty())
                return finish(code, message);
            launchThenShow(pkg, start, true, finish);
        });
        return;
    } else if (action == QLatin1String("screen.read")) {
        if (external)
            return fail(QStringLiteral("NotAvailable"), QStringLiteral("screen content is not shared with a paired computer"));
        unlocked(pkg, true, [this, pkg, fail, reply](const QString &code, const QString &message) {
            if (!code.isEmpty())
                return fail(code, message);
            notSecure(pkg, [this, pkg, fail, reply](const QString &code, const QString &message) {
                if (!code.isEmpty())
                    return fail(code, message);
                readScreen(pkg, kScreenChars,
                           [fail, reply](const QString &code, const QString &message, const QJsonObject &result) {
                               code.isEmpty() ? reply(result) : fail(code, message);
                           });
            });
        });
        return;
    } else if (action == QLatin1String("ui.tap") || action == QLatin1String("ui.type")) {
        if (!userInitiated || external)
            return fail(QStringLiteral("NotAvailable"),
                        QStringLiteral("only Pilot acts on an app's screen, for a request the person started"));
        qint64 snapshot = 0;
        int    index    = 0;
        if (!parseNode(arg("node"), &snapshot, &index))
            return fail(QStringLiteral("InvalidArguments"), QStringLiteral("node must be an id from screen.read, such as 3.17"));
        if (snapshot != entry->snapshot)
            return fail(QStringLiteral("InvalidArguments"),
                        QStringLiteral("that id is from an older screen.read: read the screen again"));
        QJsonObject act{{QStringLiteral("t"), QStringLiteral("ui_act")},
                        {QStringLiteral("pkg"), pkg},
                        {QStringLiteral("snap"), double(snapshot)},
                        {QStringLiteral("node"), index},
                        {QStringLiteral("act"), action == QLatin1String("ui.tap") ? QStringLiteral("click")
                                                                                  : QStringLiteral("set_text")}};
        if (action == QLatin1String("ui.type"))
            act.insert(QStringLiteral("text"), arg("text"));
        unlocked(pkg, true, [this, act, finish](const QString &code, const QString &message) {
            if (!code.isEmpty())
                return finish(code, message);
            request(act, [finish](bool ok, const QJsonObject &r) {
                if (!ok)
                    return finish(QStringLiteral("NotAvailable"), r.value(QStringLiteral("error")).toString());
                r.value(QStringLiteral("ok")).toBool()
                    ? finish({}, {})
                    : finish(QStringLiteral("Failed"), Validate::text(r, QStringLiteral("error"), 256));
            });
        });
        return;
    } else {
        return fail(QStringLiteral("UnknownAction"), QStringLiteral("no action named ") + action.left(64));
    }

    unlocked(pkg, false, [this, pkg, launch, finish](const QString &code, const QString &message) {
        if (!code.isEmpty())
            return finish(code, message);
        launchThenShow(pkg, launch, false, finish);
    });
}

void KeelApps::context(const QString &pkg, bool external, const QDBusMessage &call,
                       QDBusConnection connection) {
    const auto reply = [call, connection](const QJsonObject &result) {
        connection.send(call.createReply(compact(result)));
    };
    if (external || !m_apps.contains(pkg))
        return reply({});
    unlocked(pkg, true, [this, pkg, reply](const QString &code, const QString &) {
        if (!code.isEmpty())
            return reply({});
        notSecure(pkg, [this, pkg, reply](const QString &code, const QString &) {
            if (!code.isEmpty())
                return reply({});
            readScreen(pkg, kContextChars, [pkg, reply](const QString &code, const QString &, const QJsonObject &r) {
                if (!code.isEmpty())
                    return reply({});
                reply({{QStringLiteral("app"), appId(pkg)},
                       {QStringLiteral("purpose"),
                        QStringLiteral("What %1 (an Android app) shows on screen now")
                            .arg(r.value(QStringLiteral("label")).toString())},
                       {QStringLiteral("text"), r.value(QStringLiteral("screen")).toString()}});
            });
        });
    });
}
