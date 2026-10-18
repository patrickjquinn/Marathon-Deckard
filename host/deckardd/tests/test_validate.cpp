#include "validate.h"

#include <QJsonArray>
#include <QJsonDocument>
#include <QTest>

class TestValidate : public QObject {
    Q_OBJECT

  private Q_SLOTS:
    void packageNames() {
        QVERIFY(Validate::isPackageName(QStringLiteral("org.lineageos.jelly")));
        QVERIFY(Validate::isPackageName(QStringLiteral("android")));
        QVERIFY(!Validate::isPackageName(QStringLiteral("../etc/passwd")));
        QVERIFY(!Validate::isPackageName(QStringLiteral("org..x")));
        QVERIFY(!Validate::isPackageName(QStringLiteral("1abc")));
        QVERIFY(!Validate::isPackageName(QString(300, u'a')));
    }

    void textIsBoundedAndClean() {
        const QString     dirty = QStringLiteral("a") + QChar(0) + u'b' + QChar(0x202e) + QStringLiteral("c\nd");
        const QJsonObject o{{QStringLiteral("s"), dirty},
                            {QStringLiteral("n"), 42}};
        QCOMPARE(Validate::text(o, QStringLiteral("s"), 100), QStringLiteral("abc\nd"));
        QCOMPARE(Validate::text(o, QStringLiteral("s"), 2), QStringLiteral("ab"));
        QCOMPARE(Validate::text(o, QStringLiteral("n"), 10), QString());
        const QJsonObject emoji{{QStringLiteral("s"), QStringLiteral("x\U0001F600")}};
        QCOMPARE(Validate::text(emoji, QStringLiteral("s"), 2), QStringLiteral("x"));
    }

    void base64IsBounded() {
        const QJsonObject o{{QStringLiteral("b"), QString::fromLatin1(QByteArray(100, 'x').toBase64())},
                            {QStringLiteral("bad"), QStringLiteral("!!!")}};
        QCOMPARE(Validate::base64(o, QStringLiteral("b"), 100).size(), 100);
        QVERIFY(Validate::base64(o, QStringLiteral("b"), 50).isEmpty());
        QVERIFY(Validate::base64(o, QStringLiteral("bad"), 50).isEmpty());
    }

    void categories() {
        QCOMPARE(Validate::fdoCategory(QStringLiteral("msg")), QStringLiteral("im.received"));
        QCOMPARE(Validate::fdoCategory(QStringLiteral("alarm")), QStringLiteral("x-android.alarm"));
        QCOMPARE(Validate::fdoCategory(QStringLiteral("Evil Category")), QString());
        QCOMPARE(Validate::fdoCategory(QString()), QString());
    }

    void post() {
        QJsonArray actions;
        for (int i = 0; i < 9; ++i)
            actions.append(QJsonObject{{QStringLiteral("id"), QStringLiteral("a%1").arg(i)},
                                       {QStringLiteral("label"), QStringLiteral("Action %1").arg(i)}});
        actions.append(QJsonObject{{QStringLiteral("id"), QStringLiteral("default")},
                                   {QStringLiteral("label"), QStringLiteral("x")}});
        const QJsonObject m{
            {QStringLiteral("key"), QStringLiteral("0|org.lineageos.jelly|1|null|10123")},
            {QStringLiteral("pkg"), QStringLiteral("org.lineageos.jelly")},
            {QStringLiteral("label"), QStringLiteral("Jelly")},
            {QStringLiteral("title"), QStringLiteral("file.zip")},
            {QStringLiteral("text"), QStringLiteral("Downloading")},
            {QStringLiteral("content"), true},
            {QStringLiteral("urgency"), 7},
            {QStringLiteral("category"), QStringLiteral("progress")},
            {QStringLiteral("reply"), QJsonObject{{QStringLiteral("action"), QStringLiteral("a1")},
                                                  {QStringLiteral("placeholder"), QStringLiteral("Message")}}},
            {QStringLiteral("actions"), actions},
            {QStringLiteral("progress"), QJsonObject{{QStringLiteral("max"), 200},
                                                     {QStringLiteral("current"), 50}}}};
        QString error;
        const auto p = Validate::post(m, &error);
        QVERIFY2(p, qPrintable(error));
        QCOMPARE(p->urgency, 2);
        QCOMPARE(p->progress, 25);
        QCOMPARE(p->category, QStringLiteral("transfer"));
        QCOMPARE(p->replyAction, QStringLiteral("a1"));
        QCOMPARE(p->actions.mid(0, 4), (QStringList{QStringLiteral("default"), QStringLiteral("Open"),
                                                    QStringLiteral("inline-reply"), QStringLiteral("Reply")}));
        QCOMPARE(p->actions.size(), 4 + 2 * 5);
        QVERIFY(!p->actions.contains(QStringLiteral("a1")));

        QJsonObject indeterminate = m;
        indeterminate[QStringLiteral("progress")] = QJsonObject{{QStringLiteral("indeterminate"), true}};
        QCOMPARE(Validate::post(indeterminate, &error)->progress, -1);

        QJsonObject badPkg = m;
        badPkg[QStringLiteral("pkg")] = QStringLiteral("../../x");
        QVERIFY(!Validate::post(badPkg, &error));
        QJsonObject badKey = m;
        badKey[QStringLiteral("key")] = QString(600, u'k');
        QVERIFY(!Validate::post(badKey, &error));
    }

    void conversation() {
        QJsonArray messages;
        for (int i = 0; i < 15; ++i)
            messages.append(QJsonObject{{QStringLiteral("sender"), QStringLiteral("Ann")},
                                        {QStringLiteral("text"), QStringLiteral("m%1").arg(i)},
                                        {QStringLiteral("time"), 1000 + i},
                                        {QStringLiteral("extra"), QStringLiteral("dropped")}});
        const QJsonObject m{{QStringLiteral("key"), QStringLiteral("k")},
                            {QStringLiteral("pkg"), QStringLiteral("com.example")},
                            {QStringLiteral("conversation"),
                             QJsonObject{{QStringLiteral("title"), QStringLiteral("Group")},
                                         {QStringLiteral("group"), true},
                                         {QStringLiteral("messages"), messages}}}};
        QString    error;
        const auto p = Validate::post(m, &error);
        QVERIFY(p);
        const QJsonObject c = QJsonDocument::fromJson(p->conversation.toUtf8()).object();
        QCOMPARE(c.value(QStringLiteral("messages")).toArray().size(), 10);
        QCOMPARE(c.value(QStringLiteral("messages")).toArray().first().toObject().value(QStringLiteral("text")).toString(),
                 QStringLiteral("m5"));
        QVERIFY(!c.value(QStringLiteral("messages")).toArray().first().toObject().contains(QStringLiteral("extra")));
        QCOMPARE(p->label, QStringLiteral("com.example"));
    }

    void media() {
        const QJsonObject m{{QStringLiteral("sid"), 3},
                            {QStringLiteral("pkg"), QStringLiteral("org.lineageos.eleven")},
                            {QStringLiteral("state"), QStringLiteral("Dancing")},
                            {QStringLiteral("position"), -5},
                            {QStringLiteral("speed"), 1e9},
                            {QStringLiteral("can"), QJsonObject{{QStringLiteral("next"), true}}}};
        QString    error;
        const auto s = Validate::media(m, &error);
        QVERIFY(s);
        QCOMPARE(s->status, QStringLiteral("Stopped"));
        QCOMPARE(s->positionMs, 0);
        QCOMPARE(s->speed, 16.0);
        QVERIFY(s->canNext && !s->canPlay && !s->hasArt);
        QJsonObject bad = m;
        bad[QStringLiteral("sid")] = 0;
        QVERIFY(!Validate::media(bad, &error));
    }
};

QTEST_GUILESS_MAIN(TestValidate)
#include "test_validate.moc"
