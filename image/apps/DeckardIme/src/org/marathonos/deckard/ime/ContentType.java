package org.marathonos.deckard.ime;

import android.text.InputType;

/** An Android editor's input type as zwp_text_input_v3 content purpose and hints. */
final class ContentType {
    static final int PURPOSE_NORMAL = 0;
    static final int PURPOSE_DIGITS = 2;
    static final int PURPOSE_NUMBER = 3;
    static final int PURPOSE_PHONE = 4;
    static final int PURPOSE_URL = 5;
    static final int PURPOSE_EMAIL = 6;
    static final int PURPOSE_NAME = 7;
    static final int PURPOSE_PASSWORD = 8;
    static final int PURPOSE_PIN = 9;
    static final int PURPOSE_DATE = 10;
    static final int PURPOSE_TIME = 11;
    static final int PURPOSE_DATETIME = 12;
    static final int PURPOSE_TERMINAL = 13;

    static final int HINT_COMPLETION = 0x1;
    static final int HINT_SPELLCHECK = 0x2;
    static final int HINT_AUTO_CAPITALIZATION = 0x4;
    static final int HINT_UPPERCASE = 0x10;
    static final int HINT_TITLECASE = 0x20;
    static final int HINT_HIDDEN_TEXT = 0x40;
    static final int HINT_SENSITIVE_DATA = 0x80;
    static final int HINT_MULTILINE = 0x200;

    final int purpose;
    final int hints;

    private ContentType(int purpose, int hints) {
        this.purpose = purpose;
        this.hints = hints;
    }

    boolean sensitive() {
        return (hints & HINT_SENSITIVE_DATA) != 0;
    }

    static ContentType of(int inputType) {
        final int variation = inputType & InputType.TYPE_MASK_VARIATION;
        switch (inputType & InputType.TYPE_MASK_CLASS) {
            case InputType.TYPE_NULL:
                return new ContentType(PURPOSE_TERMINAL, 0);
            case InputType.TYPE_CLASS_NUMBER:
                if (variation == InputType.TYPE_NUMBER_VARIATION_PASSWORD)
                    return new ContentType(PURPOSE_PIN, HINT_HIDDEN_TEXT | HINT_SENSITIVE_DATA);
                if ((inputType & (InputType.TYPE_NUMBER_FLAG_SIGNED
                        | InputType.TYPE_NUMBER_FLAG_DECIMAL)) != 0)
                    return new ContentType(PURPOSE_NUMBER, 0);
                return new ContentType(PURPOSE_DIGITS, 0);
            case InputType.TYPE_CLASS_PHONE:
                return new ContentType(PURPOSE_PHONE, 0);
            case InputType.TYPE_CLASS_DATETIME:
                if (variation == InputType.TYPE_DATETIME_VARIATION_DATE)
                    return new ContentType(PURPOSE_DATE, 0);
                if (variation == InputType.TYPE_DATETIME_VARIATION_TIME)
                    return new ContentType(PURPOSE_TIME, 0);
                return new ContentType(PURPOSE_DATETIME, 0);
            default:
                return text(inputType, variation);
        }
    }

    private static ContentType text(int inputType, int variation) {
        int hints = 0;
        if ((inputType & InputType.TYPE_TEXT_FLAG_MULTI_LINE) != 0)
            hints |= HINT_MULTILINE;
        switch (variation) {
            case InputType.TYPE_TEXT_VARIATION_PASSWORD:
            case InputType.TYPE_TEXT_VARIATION_WEB_PASSWORD:
                return new ContentType(PURPOSE_PASSWORD, HINT_HIDDEN_TEXT | HINT_SENSITIVE_DATA);
            case InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD:
                return new ContentType(PURPOSE_PASSWORD, HINT_SENSITIVE_DATA);
            case InputType.TYPE_TEXT_VARIATION_URI:
                return new ContentType(PURPOSE_URL, hints);
            case InputType.TYPE_TEXT_VARIATION_EMAIL_ADDRESS:
            case InputType.TYPE_TEXT_VARIATION_WEB_EMAIL_ADDRESS:
                return new ContentType(PURPOSE_EMAIL, hints);
            default:
                break;
        }
        if ((inputType & InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS) == 0)
            hints |= HINT_COMPLETION | HINT_SPELLCHECK;
        if ((inputType & InputType.TYPE_TEXT_FLAG_CAP_CHARACTERS) != 0)
            hints |= HINT_UPPERCASE;
        if ((inputType & InputType.TYPE_TEXT_FLAG_CAP_WORDS) != 0)
            hints |= HINT_TITLECASE | HINT_AUTO_CAPITALIZATION;
        if ((inputType & InputType.TYPE_TEXT_FLAG_CAP_SENTENCES) != 0)
            hints |= HINT_AUTO_CAPITALIZATION;
        final int purpose = variation == InputType.TYPE_TEXT_VARIATION_PERSON_NAME
                ? PURPOSE_NAME : PURPOSE_NORMAL;
        return new ContentType(purpose, hints);
    }
}
