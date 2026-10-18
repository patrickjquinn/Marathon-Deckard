#include "ime.h"

#include <QSignalSpy>
#include <QTest>

namespace {
    QJsonObject msg(const QString &type, const QJsonObject &fields = {}) {
        QJsonObject o = fields;
        o.insert(QStringLiteral("t"), type);
        return o;
    }
    const QString kApp = QStringLiteral("waydroid.org.lineageos.jelly");
} // namespace

class TestIme : public QObject {
    Q_OBJECT

    QList<QJsonObject> m_sent;

    Ime               *make() {
        m_sent.clear();
        return new Ime([this](const QJsonObject &o) { m_sent << o; }, this);
    }

  private Q_SLOTS:
    void editorReachesTheShell() {
        Ime       *ime = make();
        QSignalSpy calls(ime, &Ime::shellCall);
        ime->handle(QStringLiteral("start"),
                    msg(QStringLiteral("start"), {{QStringLiteral("package"), QStringLiteral("org.lineageos.jelly")},
                                                  {QStringLiteral("purpose"), 5},
                                                  {QStringLiteral("hints"), 0x7}}));
        ime->handle(QStringLiteral("state"),
                    msg(QStringLiteral("state"), {{QStringLiteral("text"), QStringLiteral("abc")},
                                                  {QStringLiteral("cursor"), 99},
                                                  {QStringLiteral("anchor"), -4}}));
        ime->handle(QStringLiteral("show"), msg(QStringLiteral("show")));
        ime->handle(QStringLiteral("hide"), msg(QStringLiteral("hide")));
        ime->handle(QStringLiteral("finish"), msg(QStringLiteral("finish")));
        QCOMPARE(calls.count(), 5);
        QCOMPARE(calls.at(0).at(0).toString(), QStringLiteral("SetEditor"));
        QCOMPARE(calls.at(0).at(1).toList(), (QVariantList{kApp, 5u, 7u}));
        QCOMPARE(calls.at(1).at(1).toList(), (QVariantList{kApp, QStringLiteral("abc"), 3, 0}));
        QCOMPARE(calls.at(2).at(0).toString(), QStringLiteral("ShowPanel"));
        QCOMPARE(calls.at(3).at(0).toString(), QStringLiteral("HidePanel"));
        QCOMPARE(calls.at(4).at(0).toString(), QStringLiteral("ClearEditor"));
    }

    void rejectsUntrustedFields() {
        Ime       *ime = make();
        QSignalSpy calls(ime, &Ime::shellCall);
        ime->handle(QStringLiteral("start"),
                    msg(QStringLiteral("start"), {{QStringLiteral("package"), QStringLiteral("../evil")}}));
        ime->handle(QStringLiteral("show"), msg(QStringLiteral("show")));
        QCOMPARE(calls.count(), 0);

        ime->handle(QStringLiteral("start"),
                    msg(QStringLiteral("start"), {{QStringLiteral("package"), QStringLiteral("a.b")},
                                                  {QStringLiteral("purpose"), 99},
                                                  {QStringLiteral("hints"), -1}}));
        QCOMPARE(calls.at(0).at(1).toList(),
                 (QVariantList{QStringLiteral("waydroid.a.b"), 0u, 0x3ffu}));
        ime->handle(QStringLiteral("state"),
                    msg(QStringLiteral("state"),
                        {{QStringLiteral("text"), QString(Ime::kMaxText + 1, QLatin1Char('x'))}}));
        QCOMPARE(calls.count(), 1);
    }

    void typingReachesTheFocusedEditorOnly() {
        Ime *ime = make();
        ime->handle(QStringLiteral("start"),
                    msg(QStringLiteral("start"), {{QStringLiteral("package"), QStringLiteral("org.lineageos.jelly")}}));
        ime->commitString(kApp, QStringLiteral("the"), 3, 0);
        ime->commitString(QStringLiteral("waydroid.other"), QStringLiteral("x"), 0, 0);
        ime->key(kApp, QStringLiteral("enter"));
        ime->key(kApp, QStringLiteral("power"));
        QCOMPARE(m_sent.count(), 2);
        QCOMPARE(m_sent.at(0), msg(QStringLiteral("commit"), {{QStringLiteral("text"), QStringLiteral("the")},
                                                              {QStringLiteral("before"), 3},
                                                              {QStringLiteral("after"), 0}}));
        QCOMPARE(m_sent.at(1), msg(QStringLiteral("key"), {{QStringLiteral("key"), QStringLiteral("enter")}}));
    }

    // The keyboard can be up before the app focuses a field.
    void panelIsReplayedWhenTheEditorStarts() {
        Ime *ime = make();
        ime->panelChanged(kApp, 200, 419);
        QCOMPARE(m_sent.count(), 0);
        ime->handle(QStringLiteral("start"),
                    msg(QStringLiteral("start"), {{QStringLiteral("package"), QStringLiteral("org.lineageos.jelly")}}));
        QCOMPARE(m_sent.count(), 1);
        QCOMPARE(m_sent.at(0), msg(QStringLiteral("panel"), {{QStringLiteral("covered"), 200},
                                                             {QStringLiteral("height"), 419}}));
        ime->panelChanged(kApp, 0, 419);
        QCOMPARE(m_sent.count(), 2);
        QCOMPARE(m_sent.at(1).value(QStringLiteral("covered")).toInt(), 0);
    }

    void disconnectClearsTheEditor() {
        Ime *ime = make();
        ime->handle(QStringLiteral("start"),
                    msg(QStringLiteral("start"), {{QStringLiteral("package"), QStringLiteral("org.lineageos.jelly")}}));
        QSignalSpy calls(ime, &Ime::shellCall);
        ime->setConnected(false);
        QCOMPARE(calls.count(), 2);
        QCOMPARE(calls.at(1).at(0).toString(), QStringLiteral("ClearEditor"));
        ime->commitString(kApp, QStringLiteral("x"), 0, 0);
        QCOMPARE(m_sent.count(), 0);
    }
};

QTEST_GUILESS_MAIN(TestIme)
#include "test_ime.moc"
