#include "sharedstorage.h"

#include <QDir>
#include <QFile>
#include <QJsonArray>
#include <QTemporaryDir>
#include <QTest>

#include <sys/stat.h>
#include <sys/xattr.h>
#include <unistd.h>

class TestSharedStorage : public QObject {
    Q_OBJECT

    // (tag, perm, id) entries of a POSIX ACL xattr.
    static QList<std::tuple<int, int, quint32>> acl(const QString &path, const char *name) {
        QByteArray buf(256, 0);
        const ssize_t n = ::lgetxattr(QFile::encodeName(path).constData(), name, buf.data(), size_t(buf.size()));
        QList<std::tuple<int, int, quint32>> out;
        for (ssize_t i = 4; n > 0 && i + 8 <= n; i += 8) {
            const auto u8 = [&](ssize_t k) { return quint32(quint8(buf[k])); };
            out.append({int(u8(i) | u8(i + 1) << 8), int(u8(i + 2) | u8(i + 3) << 8),
                        u8(i + 4) | u8(i + 5) << 8 | u8(i + 6) << 16 | u8(i + 7) << 24});
        }
        return out;
    }

    static void touch(const QString &path) {
        QFile f(path);
        QVERIFY(f.open(QIODevice::WriteOnly));
        f.write("x");
    }

  private Q_SLOTS:
    void grantsMediaRw() {
        QTemporaryDir dir;
        const QString file = dir.filePath(QStringLiteral("photo.jpg"));
        touch(file);
        QFile::setPermissions(file, QFile::ReadOwner | QFile::WriteOwner | QFile::ReadGroup | QFile::ReadOther);
        if (!SharedStorage::grant(file, false))
            QSKIP("no POSIX ACLs here");
        const auto access = acl(file, "system.posix_acl_access");
        QVERIFY(access.contains(std::make_tuple(0x08, 6, quint32(SharedStorage::kMediaRw))));
        QVERIFY(access.contains(std::make_tuple(0x20, 4, quint32(0xffffffff))));
        struct stat st {};
        QCOMPARE(::stat(QFile::encodeName(file).constData(), &st), 0);
        QCOMPARE(st.st_mode & 0002, 0u);

        QVERIFY(SharedStorage::grant(dir.path(), true));
        const auto defaults = acl(dir.path(), "system.posix_acl_default");
        QVERIFY(defaults.contains(std::make_tuple(0x08, 7, quint32(SharedStorage::kMediaRw))));
        QVERIFY(defaults.contains(std::make_tuple(0x02, 7, quint32(::getuid()))));

        // A file made in the folder inherits read and write for media_rw.
        const QString child = dir.filePath(QStringLiteral("new.txt"));
        touch(child);
        QVERIFY(acl(child, "system.posix_acl_access").contains(std::make_tuple(0x08, 7, quint32(SharedStorage::kMediaRw))));

        QVERIFY(QFile::link(file, dir.filePath(QStringLiteral("link"))));
        QVERIFY(!SharedStorage::grant(dir.filePath(QStringLiteral("link")), false));
    }

    void mapsToAndroidPaths() {
        SharedStorage s({{QStringLiteral("/home/u/Pictures"), QStringLiteral("Pictures")},
                         {QStringLiteral("/home/u/Videos"), QStringLiteral("Movies")}},
                        QString(), [](const QJsonObject &) { return true; });
        QCOMPARE(s.androidPath(QStringLiteral("/home/u/Pictures")), QStringLiteral("/storage/emulated/0/Pictures"));
        QCOMPARE(s.androidPath(QStringLiteral("/home/u/Videos/a/b.mp4")), QStringLiteral("/storage/emulated/0/Movies/a/b.mp4"));
        QCOMPARE(s.androidPath(QStringLiteral("/home/u/Pictures/../.ssh")), QString());
        QCOMPARE(s.androidPath(QStringLiteral("/home/u/PicturesX")), QString());
    }

    void indexesWhatTheHostChanges() {
        QTemporaryDir home;
        const QString pictures = home.filePath(QStringLiteral("Pictures"));
        QVERIFY(QDir().mkpath(pictures + QStringLiteral("/Marathon/old")));
        touch(pictures + QStringLiteral("/Marathon/old/a.jpg"));
        QList<QJsonObject> sent;
        const QString stamp = home.filePath(QStringLiteral("state/shared-acl"));
        SharedStorage s({{pictures, QStringLiteral("Pictures")}, {home.filePath(QStringLiteral("DCIM")), QStringLiteral("DCIM")}},
                        stamp, [&](const QJsonObject &m) { sent.append(m); return true; });
        s.prepare();
        QVERIFY(QFileInfo(home.filePath(QStringLiteral("DCIM"))).isDir());
        const bool acls = QFile::exists(stamp);
        if (acls)
            QVERIFY(acl(pictures + QStringLiteral("/Marathon/old/a.jpg"), "system.posix_acl_access")
                        .contains(std::make_tuple(0x08, 6, quint32(SharedStorage::kMediaRw))));

        // Changes while Android is away wait for it.
        touch(pictures + QStringLiteral("/Marathon/b.jpg"));
        QTest::qWait(SharedStorage::kSettleMs + 500);
        QVERIFY(sent.isEmpty());

        s.setConnected(true);
        QCOMPARE(sent.size(), 1);
        QCOMPARE(sent[0].value(QStringLiteral("t")).toString(), QStringLiteral("media_scan"));
        QStringList paths;
        for (const QJsonValue &v : sent[0].value(QStringLiteral("paths")).toArray())
            paths << v.toString();
        paths.sort();
        QCOMPARE(paths, (QStringList{QStringLiteral("/storage/emulated/0/DCIM"), QStringLiteral("/storage/emulated/0/Pictures"),
                                     QStringLiteral("/storage/emulated/0/Pictures/Marathon")}));

        touch(pictures + QStringLiteral("/Marathon/c.jpg"));
        QTRY_COMPARE_WITH_TIMEOUT(sent.size(), 2, SharedStorage::kSettleMs + 2000);
        QCOMPARE(sent[1].value(QStringLiteral("paths")).toArray(),
                 QJsonArray{QStringLiteral("/storage/emulated/0/Pictures/Marathon")});
        if (acls)
            QVERIFY(acl(pictures + QStringLiteral("/Marathon/c.jpg"), "system.posix_acl_access")
                        .contains(std::make_tuple(0x08, 6, quint32(SharedStorage::kMediaRw)))
                    || acl(pictures + QStringLiteral("/Marathon/c.jpg"), "system.posix_acl_access")
                           .contains(std::make_tuple(0x08, 7, quint32(SharedStorage::kMediaRw))));
    }
};

QTEST_GUILESS_MAIN(TestSharedStorage)
#include "test_sharedstorage.moc"
