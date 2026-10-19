#include "sharedstorage.h"

#include <QDateTime>
#include <QDir>
#include <QDirIterator>
#include <QFile>
#include <QFileInfo>
#include <QJsonArray>
#include <QLoggingCategory>
#include <QStandardPaths>

#include <algorithm>

#include <sys/stat.h>
#include <sys/xattr.h>
#include <unistd.h>

Q_LOGGING_CATEGORY(lcShared, "deckard.storage")

namespace {
    // The kernel's POSIX ACL xattr (linux/posix_acl_xattr.h): a version,
    // then (tag, perm, id) entries sorted by tag and id, little-endian.
    constexpr quint32 kAclVersion = 2;
    constexpr quint16 kUserObj    = 0x01;
    constexpr quint16 kUser       = 0x02;
    constexpr quint16 kGroupObj   = 0x04;
    constexpr quint16 kGroup      = 0x08;
    constexpr quint16 kMask       = 0x10;
    constexpr quint16 kOther      = 0x20;
    constexpr quint32 kNoId       = 0xffffffff;

    struct Entry {
        quint16 tag;
        quint16 perm;
        quint32 id;
    };

    void put(QByteArray &out, quint32 value, int bytes) {
        for (int i = 0; i < bytes; ++i)
            out.append(char((value >> (8 * i)) & 0xff));
    }

    QByteArray encode(QList<Entry> entries) {
        std::sort(entries.begin(), entries.end(),
                  [](const Entry &a, const Entry &b) { return a.tag != b.tag ? a.tag < b.tag : a.id < b.id; });
        QByteArray out;
        put(out, kAclVersion, 4);
        for (const Entry &e : entries) {
            put(out, e.tag, 2);
            put(out, e.perm, 2);
            put(out, e.id, 4);
        }
        return out;
    }

    // Whether the access ACL already lets media_rw in: writing it again
    // would wake the folder's watcher.
    bool granted(const QByteArray &path) {
        char         buf[512];
        const ssize_t n = ::lgetxattr(path.constData(), "system.posix_acl_access", buf, sizeof buf);
        for (ssize_t i = 4; i + 8 <= n; i += 8) {
            const auto b   = [&](ssize_t k) { return quint32(quint8(buf[k])); };
            const quint32 tag = b(i) | b(i + 1) << 8;
            const quint32 id  = b(i + 4) | b(i + 5) << 8 | b(i + 6) << 16 | b(i + 7) << 24;
            if (tag == kGroup && id == SharedStorage::kMediaRw)
                return true;
        }
        return false;
    }

    bool set(const QByteArray &path, const char *name, const QByteArray &acl) {
        return ::lsetxattr(path.constData(), name, acl.constData(), size_t(acl.size()), 0) == 0;
    }
} // namespace

QList<SharedStorage::Folder> SharedStorage::userFolders() {
    const QString home = QDir::homePath();
    QList<Folder> out;
    const QList<std::pair<QStandardPaths::StandardLocation, QString>> xdg{
        {QStandardPaths::DocumentsLocation, QStringLiteral("Documents")},
        {QStandardPaths::DownloadLocation, QStringLiteral("Download")},
        {QStandardPaths::PicturesLocation, QStringLiteral("Pictures")},
        {QStandardPaths::MusicLocation, QStringLiteral("Music")},
        {QStandardPaths::MoviesLocation, QStringLiteral("Movies")}};
    for (const auto &[location, android] : xdg) {
        const QString dir = QStandardPaths::writableLocation(location);
        // The hook shares only folders directly in the home.
        if (!dir.isEmpty() && QFileInfo(dir).path() == home)
            out.append({dir, android});
    }
    out.append({home + QStringLiteral("/DCIM"), QStringLiteral("DCIM")});
    return out;
}

SharedStorage::SharedStorage(QList<Folder> folders, QString stamp, Send send, QObject *parent)
    : QObject(parent)
    , m_folders(std::move(folders))
    , m_stamp(std::move(stamp))
    , m_send(std::move(send)) {
    m_settle.setSingleShot(true);
    m_settle.setInterval(kSettleMs);
    connect(&m_settle, &QTimer::timeout, this, &SharedStorage::flush);
    connect(&m_watcher, &QFileSystemWatcher::directoryChanged, this, &SharedStorage::changed);
}

bool SharedStorage::grant(const QString &path, bool directory) {
    const QByteArray p = QFile::encodeName(path);
    struct stat st {};
    if (::lstat(p.constData(), &st) != 0 || S_ISLNK(st.st_mode) || st.st_uid != ::getuid())
        return false;
    if (granted(p))
        return true;
    const quint16 owner = (st.st_mode >> 6) & 7;
    const quint16 group = (st.st_mode >> 3) & 7;
    const quint16 other = st.st_mode & 7;
    const quint16 media = directory ? 7 : 6;
    if (!set(p, "system.posix_acl_access",
             encode({{kUserObj, owner, kNoId}, {kGroupObj, group, kNoId}, {kGroup, media, kMediaRw},
                     {kMask, quint16(group | media), kNoId}, {kOther, other, kNoId}})))
        return false;
    if (!directory)
        return true;
    // What MediaProvider creates here stays the user's to read and change.
    return set(p, "system.posix_acl_default",
               encode({{kUserObj, 7, kNoId}, {kUser, 7, quint32(::getuid())}, {kGroupObj, group, kNoId},
                       {kGroup, 7, kMediaRw}, {kMask, 7, kNoId}, {kOther, other, kNoId}}));
}

void SharedStorage::walk(const QString &dir) {
    QDirIterator it(dir, QDir::AllEntries | QDir::NoDotAndDotDot | QDir::Hidden | QDir::NoSymLinks | QDir::System,
                    QDirIterator::Subdirectories);
    while (it.hasNext()) {
        const QFileInfo info = it.nextFileInfo();
        if (info.isDir() || info.isFile())
            grant(info.filePath(), info.isDir());
    }
}

void SharedStorage::prepare() {
    const bool first = !QFile::exists(m_stamp);
    bool       ok    = true;
    for (const Folder &f : std::as_const(m_folders)) {
        if (!QFileInfo(f.host).isDir() && !QDir().mkpath(f.host))
            continue;
        if (!grant(f.host, true)) {
            qCWarning(lcShared) << "no ACL on" << f.host << "(Android may not write there)";
            ok = false;
            continue;
        }
        if (first)
            walk(f.host);
    }
    if (first && ok) {
        QDir().mkpath(QFileInfo(m_stamp).path());
        QFile stamp(m_stamp);
        if (stamp.open(QIODevice::WriteOnly))
            stamp.write("1\n");
    }
    watch();
}

void SharedStorage::watch() {
    QStringList dirs;
    for (const Folder &f : std::as_const(m_folders)) {
        dirs << f.host;
        const QFileInfoList subs = QDir(f.host).entryInfoList(QDir::Dirs | QDir::NoDotAndDotDot | QDir::NoSymLinks);
        for (const QFileInfo &s : subs)
            dirs << s.filePath();
    }
    const QStringList watched = m_watcher.directories();
    QStringList       add;
    for (const QString &d : std::as_const(dirs)) {
        if (!watched.contains(d))
            add << d;
    }
    if (!add.isEmpty())
        m_watcher.addPaths(add);
}

QString SharedStorage::androidPath(const QString &hostPath) const {
    const QString clean = QDir::cleanPath(hostPath);
    for (const Folder &f : std::as_const(m_folders)) {
        if (clean == f.host)
            return QStringLiteral("/storage/emulated/0/") + f.android;
        if (clean.startsWith(f.host + u'/'))
            return QStringLiteral("/storage/emulated/0/") + f.android + clean.mid(f.host.size());
    }
    return {};
}

void SharedStorage::changed(const QString &dir) {
    // New entries from the host get the ACL too; what Android made has the
    // folder's default already.
    const QDateTime recent = QDateTime::currentDateTime().addSecs(-2 * kSettleMs / 1000 - 2);
    const QFileInfoList entries = QDir(dir).entryInfoList(QDir::AllEntries | QDir::NoDotAndDotDot | QDir::Hidden |
                                                          QDir::NoSymLinks | QDir::System);
    for (const QFileInfo &e : entries) {
        if (e.metadataChangeTime() >= recent)
            grant(e.filePath(), e.isDir());
    }
    m_dirty.insert(dir);
    m_settle.start();
    watch();
}

void SharedStorage::flush() {
    if (!m_connected || m_dirty.isEmpty())
        return;
    QJsonArray paths;
    for (const QString &d : std::as_const(m_dirty)) {
        const QString path = androidPath(d);
        if (!path.isEmpty() && paths.size() < kMaxPaths)
            paths.append(path);
    }
    m_dirty.clear();
    if (!paths.isEmpty())
        m_send({{QStringLiteral("t"), QStringLiteral("media_scan")}, {QStringLiteral("paths"), paths}});
}

void SharedStorage::setConnected(bool connected) {
    m_connected = connected;
    if (!connected)
        return;
    for (const Folder &f : std::as_const(m_folders))
        m_dirty.insert(f.host);
    flush();
}
