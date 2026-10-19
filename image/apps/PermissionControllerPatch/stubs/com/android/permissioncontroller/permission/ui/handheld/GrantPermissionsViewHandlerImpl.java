package com.android.permissioncontroller.permission.ui.handheld;

import android.app.Activity;
import android.graphics.drawable.Icon;
import android.os.Bundle;
import android.os.UserHandle;
import android.view.View;
import android.view.WindowManager;

import com.android.permissioncontroller.permission.ui.GrantPermissionsViewHandler;

public final class GrantPermissionsViewHandlerImpl implements GrantPermissionsViewHandler {
    public GrantPermissionsViewHandlerImpl(Activity activity, String packageName, UserHandle user) {
        throw new RuntimeException("stub");
    }

    public GrantPermissionsViewHandlerImpl setResultListener(ResultListener listener) {
        throw new RuntimeException("stub");
    }

    public View createView() {
        throw new RuntimeException("stub");
    }

    public void loadInstanceState(Bundle state) {
        throw new RuntimeException("stub");
    }

    public void onBackPressed() {
        throw new RuntimeException("stub");
    }

    public void saveInstanceState(Bundle state) {
        throw new RuntimeException("stub");
    }

    public void updateUi(String groupName, int groupCount, int groupIndex, Icon icon, CharSequence message,
            CharSequence detailMessage, boolean[] buttonVisibilities, boolean[] locationVisibilities) {
        throw new RuntimeException("stub");
    }

    public void updateWindowAttributes(WindowManager.LayoutParams params) {
        throw new RuntimeException("stub");
    }
}
