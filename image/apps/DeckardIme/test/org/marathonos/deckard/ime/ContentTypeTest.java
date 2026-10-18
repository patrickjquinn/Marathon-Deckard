package org.marathonos.deckard.ime;

import android.text.InputType;

// Runs on the build host's JVM: ContentType only uses InputType constants.
public final class ContentTypeTest {
    private static int failures;

    private static void check(String name, int inputType, int purpose, int hints) {
        final ContentType t = ContentType.of(inputType);
        if (t.purpose != purpose || t.hints != hints) {
            failures++;
            System.err.printf("FAIL %s: purpose %d hints 0x%x, want %d 0x%x%n",
                    name, t.purpose, t.hints, purpose, hints);
        }
    }

    public static void main(String[] args) {
        final int text = InputType.TYPE_CLASS_TEXT;
        final int suggest = ContentType.HINT_COMPLETION | ContentType.HINT_SPELLCHECK;
        check("plain text", text, ContentType.PURPOSE_NORMAL, suggest);
        check("sentence caps", text | InputType.TYPE_TEXT_FLAG_CAP_SENTENCES,
                ContentType.PURPOSE_NORMAL, suggest | ContentType.HINT_AUTO_CAPITALIZATION);
        check("no suggestions", text | InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS,
                ContentType.PURPOSE_NORMAL, 0);
        check("multi-line", text | InputType.TYPE_TEXT_FLAG_MULTI_LINE,
                ContentType.PURPOSE_NORMAL, suggest | ContentType.HINT_MULTILINE);
        check("url", text | InputType.TYPE_TEXT_VARIATION_URI, ContentType.PURPOSE_URL, 0);
        check("email", text | InputType.TYPE_TEXT_VARIATION_EMAIL_ADDRESS,
                ContentType.PURPOSE_EMAIL, 0);
        check("web email", text | InputType.TYPE_TEXT_VARIATION_WEB_EMAIL_ADDRESS,
                ContentType.PURPOSE_EMAIL, 0);
        check("password", text | InputType.TYPE_TEXT_VARIATION_PASSWORD,
                ContentType.PURPOSE_PASSWORD,
                ContentType.HINT_HIDDEN_TEXT | ContentType.HINT_SENSITIVE_DATA);
        check("visible password", text | InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD,
                ContentType.PURPOSE_PASSWORD, ContentType.HINT_SENSITIVE_DATA);
        check("person name", text | InputType.TYPE_TEXT_VARIATION_PERSON_NAME
                        | InputType.TYPE_TEXT_FLAG_CAP_WORDS, ContentType.PURPOSE_NAME,
                suggest | ContentType.HINT_TITLECASE | ContentType.HINT_AUTO_CAPITALIZATION);
        check("digits", InputType.TYPE_CLASS_NUMBER, ContentType.PURPOSE_DIGITS, 0);
        check("decimal", InputType.TYPE_CLASS_NUMBER | InputType.TYPE_NUMBER_FLAG_DECIMAL,
                ContentType.PURPOSE_NUMBER, 0);
        check("pin", InputType.TYPE_CLASS_NUMBER | InputType.TYPE_NUMBER_VARIATION_PASSWORD,
                ContentType.PURPOSE_PIN,
                ContentType.HINT_HIDDEN_TEXT | ContentType.HINT_SENSITIVE_DATA);
        check("phone", InputType.TYPE_CLASS_PHONE, ContentType.PURPOSE_PHONE, 0);
        check("date", InputType.TYPE_CLASS_DATETIME | InputType.TYPE_DATETIME_VARIATION_DATE,
                ContentType.PURPOSE_DATE, 0);
        check("time", InputType.TYPE_CLASS_DATETIME | InputType.TYPE_DATETIME_VARIATION_TIME,
                ContentType.PURPOSE_TIME, 0);
        check("raw keys", InputType.TYPE_NULL, ContentType.PURPOSE_TERMINAL, 0);
        if (failures > 0)
            System.exit(1);
        System.out.println("ContentTypeTest: ok");
    }
}
