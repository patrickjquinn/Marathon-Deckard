#include "intents.h"

#include <QJsonArray>
#include <QTest>

using Intents::Target;

class TestIntents : public QObject {
    Q_OBJECT

    static std::optional<Target> open(const QJsonObject &m) {
        QString error;
        return Intents::target(m, &error);
    }

  private Q_SLOTS:
    void links() {
        auto t = open({{QStringLiteral("kind"), QStringLiteral("url")},
                       {QStringLiteral("uri"), QStringLiteral("https://f-droid.org/packages/")}});
        QVERIFY(t);
        QCOMPARE(t->kind, Target::Keel);
        QCOMPARE(t->app, QStringLiteral("browser"));
        QCOMPARE(t->name, QStringLiteral("browser.open"));
        QCOMPARE(t->args.value(QStringLiteral("url")).toString(), QStringLiteral("https://f-droid.org/packages/"));
        QVERIFY(!open({{QStringLiteral("kind"), QStringLiteral("url")},
                       {QStringLiteral("uri"), QStringLiteral("javascript:alert(1)")}}));
        QVERIFY(!open({{QStringLiteral("kind"), QStringLiteral("url")},
                       {QStringLiteral("uri"), QStringLiteral("file:///etc/passwd")}}));
    }

    void numbers() {
        auto t = open({{QStringLiteral("kind"), QStringLiteral("tel")},
                       {QStringLiteral("uri"), QStringLiteral("tel:+353%2087%20123-4567")}});
        QVERIFY(t);
        QCOMPARE(t->kind, Target::Route);
        QCOMPARE(t->app, QStringLiteral("phone"));
        QCOMPARE(t->name, QStringLiteral("/call"));
        QCOMPARE(t->args.value(QStringLiteral("number")).toString(), QStringLiteral("+353871234567"));

        t = open({{QStringLiteral("kind"), QStringLiteral("tel")}, {QStringLiteral("uri"), QStringLiteral("tel:")}});
        QVERIFY(t);
        QCOMPARE(t->kind, Target::Launch);

        t = open({{QStringLiteral("kind"), QStringLiteral("sms")},
                  {QStringLiteral("uri"), QStringLiteral("smsto:0871234567?body=hi")}});
        QVERIFY(t);
        QCOMPARE(t->app, QStringLiteral("messages"));
        QCOMPARE(t->name, QStringLiteral("conversation"));
        QCOMPARE(t->args.value(QStringLiteral("number")).toString(), QStringLiteral("0871234567"));

        t = open({{QStringLiteral("kind"), QStringLiteral("tel")},
                  {QStringLiteral("uri"), QStringLiteral("tel:$(reboot)")}});
        QVERIFY(t);
        QCOMPARE(t->kind, Target::Launch);
        QVERIFY(!open({{QStringLiteral("kind"), QStringLiteral("tel")},
                       {QStringLiteral("uri"), QStringLiteral("sms:1")}}));
    }

    void mail() {
        auto t = open({{QStringLiteral("kind"), QStringLiteral("mailto")},
                       {QStringLiteral("uri"), QStringLiteral("mailto:a@b.ie")},
                       {QStringLiteral("to"), QJsonArray{QStringLiteral("a@b.ie"), QStringLiteral("bad address")}},
                       {QStringLiteral("subject"), QStringLiteral("Hi")}});
        QVERIFY(t);
        QCOMPARE(t->app, QStringLiteral("email"));
        QCOMPARE(t->name, QStringLiteral("mail.compose"));
        QCOMPARE(t->args.value(QStringLiteral("to")).toString(), QStringLiteral("a@b.ie"));
        QCOMPARE(t->args.value(QStringLiteral("subject")).toString(), QStringLiteral("Hi"));
    }

    void places() {
        auto t = open({{QStringLiteral("kind"), QStringLiteral("geo")},
                       {QStringLiteral("uri"), QStringLiteral("geo:53.3498,-6.2603")}});
        QVERIFY(t);
        QCOMPARE(t->name, QStringLiteral("map.show"));
        QCOMPARE(t->args.value(QStringLiteral("lat")).toDouble(), 53.3498);

        t = open({{QStringLiteral("kind"), QStringLiteral("geo")},
                  {QStringLiteral("uri"), QStringLiteral("geo:0,0?q=53.34,-6.26(Trinity%20College)")}});
        QVERIFY(t);
        QCOMPARE(t->name, QStringLiteral("map.show"));
        QCOMPARE(t->args.value(QStringLiteral("name")).toString(), QStringLiteral("Trinity College"));

        t = open({{QStringLiteral("kind"), QStringLiteral("geo")},
                  {QStringLiteral("uri"), QStringLiteral("geo:0,0?q=coffee")}});
        QVERIFY(t);
        QCOMPARE(t->kind, Target::Launch);
        QCOMPARE(t->app, QStringLiteral("maps"));
    }

    void settings() {
        QCOMPARE(Intents::settingsRoute(QStringLiteral("android.settings.WIFI_SETTINGS"), {}), QStringLiteral("wifi"));
        QCOMPARE(Intents::settingsRoute(QStringLiteral("android.settings.APPLICATION_DETAILS_SETTINGS"),
                                        QStringLiteral("org.fdroid.fdroid")),
                 QStringLiteral("appmanager/waydroid.org.fdroid.fdroid"));
        QCOMPARE(Intents::settingsRoute(QStringLiteral("android.settings.APP_NOTIFICATION_SETTINGS"),
                                        QStringLiteral("../x")),
                 QStringLiteral("notifications"));
        QVERIFY(Intents::settingsRoute(QStringLiteral("android.settings.LOCATION_SOURCE_SETTINGS"), {}).isEmpty());
        auto t = open({{QStringLiteral("kind"), QStringLiteral("settings")},
                       {QStringLiteral("action"), QStringLiteral("android.settings.SETTINGS")}});
        QVERIFY(t);
        QCOMPARE(t->kind, Target::Launch);
        QCOMPARE(t->app, QStringLiteral("settings"));
    }

    void namesAndIds() {
        QVERIFY(Intents::isRequestId(QStringLiteral("0123456789abcdef")));
        QVERIFY(!Intents::isRequestId(QStringLiteral("../0123456789abc")));
        QCOMPARE(Intents::safeName(QStringLiteral("/home/user/Documents/report.pdf")), QStringLiteral("report.pdf"));
        QCOMPARE(Intents::safeName(QStringLiteral("..")), QStringLiteral("file"));
        QCOMPARE(Intents::safeName(QStringLiteral("a\nb.txt")), QStringLiteral("ab.txt"));
    }
};

QTEST_GUILESS_MAIN(TestIntents)
#include "test_intents.moc"
