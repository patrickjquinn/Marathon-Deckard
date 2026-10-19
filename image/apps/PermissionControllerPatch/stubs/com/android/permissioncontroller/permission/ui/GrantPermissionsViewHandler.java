package com.android.permissioncontroller.permission.ui;

import android.graphics.drawable.Icon;
import android.os.Bundle;
import android.view.View;
import android.view.Window;
import android.view.WindowManager;

import java.util.List;

public interface GrantPermissionsViewHandler {
    interface ResultListener {
        void onPermissionGrantResult(String name, int result);

        void onPermissionGrantResult(String name, List<String> affectedForegroundPermissions, int result);
    }

    View createView();

    void loadInstanceState(Bundle state);

    void onBackPressed();

    default void onBlurEnabledChanged(Window window, boolean enabled) {
    }

    void saveInstanceState(Bundle state);

    void updateUi(String groupName, int groupCount, int groupIndex, Icon icon, CharSequence message,
            CharSequence detailMessage, boolean[] buttonVisibilities, boolean[] locationVisibilities);

    void updateWindowAttributes(WindowManager.LayoutParams params);
}
