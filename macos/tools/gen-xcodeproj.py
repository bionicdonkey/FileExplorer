#!/usr/bin/env python3
"""Regenerates MorinExplorer.xcodeproj and its shared scheme. Run from anywhere:

    python3 macos/tools/gen-xcodeproj.py

The generated files are committed. Source files are never listed in the project: each target
owns a folder (a folder-synchronized group), so adding a file to the folder adds it to the target.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from xcodeproj_lib import build, oid  # noqa: E402

PROJECT = "MorinExplorer"
APP = "Morin Explorer"

SPEC = {
    "name": PROJECT,
    "packages": [
        {"path": "Packages/ExplorerDomain", "products": ["ExplorerDomain"]},
        {"path": "Packages/ExplorerAdapters", "products": ["ExplorerAdapters"]},
    ],
    "project_settings": {
        "SDKROOT": "macosx",
        "MACOSX_DEPLOYMENT_TARGET": "14.0",
        "SWIFT_VERSION": "5.0",
        # Universal: one build that runs on Intel and Apple silicon (plan D4).
        "ARCHS": "arm64 x86_64",
        "ONLY_ACTIVE_ARCH": "NO",
        "CODE_SIGN_STYLE": "Manual",
        "CODE_SIGN_IDENTITY": "-",
        "CLANG_ENABLE_MODULES": "YES",
        "ENABLE_USER_SCRIPT_SANDBOXING": "NO",
        "ENABLE_HARDENED_RUNTIME": "YES",
        "MARKETING_VERSION": "0.1.0",
        "CURRENT_PROJECT_VERSION": "1",
        # Set to the signing team's identifier for release builds; empty pins only identifiers.
        "MORIN_TEAM_ID": "",
    },
    "project_settings_debug": {
        "ONLY_ACTIVE_ARCH": "YES",
        "ENABLE_TESTABILITY": "YES",
        "SWIFT_OPTIMIZATION_LEVEL": "-Onone",
        "SWIFT_ACTIVE_COMPILATION_CONDITIONS": "DEBUG",
    },
    "targets": [
        {
            "name": "MorinExplorerHelper", "kind": "tool", "folders": ["Helper"],
            "exclude": ["casa.alera.MorinExplorer.helper.plist"],
            "products": ["ExplorerDomain", "ExplorerAdapters"],
            "settings": {
                "PRODUCT_NAME": "MorinExplorerHelper",
                "PRODUCT_MODULE_NAME": "MorinExplorerHelper",
                "PRODUCT_BUNDLE_IDENTIFIER": "casa.alera.MorinExplorer.helper",
                "SKIP_INSTALL": "YES",
                "GENERATE_INFOPLIST_FILE": "YES",
                "CREATE_INFOPLIST_SECTION_IN_BINARY": "YES",
                "INFOPLIST_KEY_MorinTeamIdentifier": "$(MORIN_TEAM_ID)",
                "ENABLE_HARDENED_RUNTIME": "YES",
            },
        },
        {
            "name": "MorinExplorer", "kind": "app", "folders": ["App"], "product_name": APP,
            "depends": ["MorinExplorerHelper"],
            "scripts": [{"name": "Embed the privileged helper", "body": "set -e\nlib=\"$TARGET_BUILD_DIR/$CONTENTS_FOLDER_PATH/Library/LaunchDaemons\"\nmkdir -p \"$lib\"\ncp \"$BUILT_PRODUCTS_DIR/MorinExplorerHelper\" \"$TARGET_BUILD_DIR/$EXECUTABLE_FOLDER_PATH/MorinExplorerHelper\"\ncp \"$SRCROOT/Helper/casa.alera.MorinExplorer.helper.plist\" \"$lib/\""}],
            "products": ["ExplorerDomain", "ExplorerAdapters"],
            "settings": {
                "PRODUCT_BUNDLE_IDENTIFIER": "casa.alera.MorinExplorer",
                "PRODUCT_NAME": APP,
                "PRODUCT_MODULE_NAME": "MorinExplorer",
                "INFOPLIST_KEY_MorinTeamIdentifier": "$(MORIN_TEAM_ID)",
                "GENERATE_INFOPLIST_FILE": "YES",
                "INFOPLIST_KEY_CFBundleDisplayName": APP,
                "INFOPLIST_KEY_NSPrincipalClass": "NSApplication",
                "INFOPLIST_KEY_LSMinimumSystemVersion": "14.0",
                "INFOPLIST_KEY_LSApplicationCategoryType": "public.app-category.utilities",
                "INFOPLIST_KEY_NSHumanReadableCopyright": "GPL-3.0-or-later",
                "ENABLE_APP_SANDBOX": "NO",
            },
        },
        {
            "name": "MorinExplorerUnitTests", "kind": "unit", "folders": ["Tests/UnitTests"],
            "depends": ["MorinExplorer"], "products": ["ExplorerDomain", "ExplorerAdapters"],
            "settings": {
                "PRODUCT_BUNDLE_IDENTIFIER": "casa.alera.MorinExplorer.unit",
                "PRODUCT_NAME": "MorinExplorerUnitTests",
                "GENERATE_INFOPLIST_FILE": "YES",
                "BUNDLE_LOADER": "$(TEST_HOST)",
                "TEST_HOST": "$(BUILT_PRODUCTS_DIR)/%s.app/Contents/MacOS/%s" % (APP, APP),
            },
        },
        {
            "name": "MorinExplorerUITests", "kind": "ui", "folders": ["Tests/UITests"],
            "depends": ["MorinExplorer"],
            "settings": {
                "PRODUCT_BUNDLE_IDENTIFIER": "casa.alera.MorinExplorer.ui",
                "PRODUCT_NAME": "MorinExplorerUITests",
                "GENERATE_INFOPLIST_FILE": "YES",
                "TEST_TARGET_NAME": APP,
            },
        },
    ],
}


def ref(target, built):
    return (
        '<BuildableReference BuildableIdentifier = "primary" BlueprintIdentifier = "%s" '
        'BuildableName = "%s" BlueprintName = "%s" ReferencedContainer = "container:%s.xcodeproj">'
        "</BuildableReference>" % (oid("target" + target), built, target, PROJECT)
    )


def scheme():
    app = ref("MorinExplorer", APP + ".app")
    unit = ref("MorinExplorerUnitTests", "MorinExplorerUnitTests.xctest")
    ui = ref("MorinExplorerUITests", "MorinExplorerUITests.xctest")
    return f"""<?xml version="1.0" encoding="UTF-8"?>
<Scheme LastUpgradeVersion = "1600" version = "1.7">
  <BuildAction parallelizeBuildables = "YES" buildImplicitDependencies = "YES">
    <BuildActionEntries>
      <BuildActionEntry buildForTesting = "YES" buildForRunning = "YES" buildForProfiling = "YES" buildForArchiving = "YES" buildForAnalyzing = "YES">{app}</BuildActionEntry>
    </BuildActionEntries>
  </BuildAction>
  <TestAction buildConfiguration = "Debug" selectedDebuggerIdentifier = "Xcode.DebuggerFoundation.Debugger.LLDB" selectedLauncherIdentifier = "Xcode.DebuggerFoundation.Launcher.LLDB" shouldUseLaunchSchemeArgsEnv = "YES">
    <Testables>
      <TestableReference skipped = "NO">{unit}</TestableReference>
      <TestableReference skipped = "NO">{ui}</TestableReference>
    </Testables>
  </TestAction>
  <LaunchAction buildConfiguration = "Debug" selectedDebuggerIdentifier = "Xcode.DebuggerFoundation.Debugger.LLDB" selectedLauncherIdentifier = "Xcode.DebuggerFoundation.Launcher.LLDB" launchStyle = "0" useCustomWorkingDirectory = "NO" ignoresPersistentStateOnLaunch = "NO" debugDocumentVersioning = "YES" debugServiceExtension = "internal" allowLocationSimulation = "YES">
    <BuildableProductRunnable runnableDebuggingMode = "0">{app}</BuildableProductRunnable>
  </LaunchAction>
  <ProfileAction buildConfiguration = "Release" shouldUseLaunchSchemeArgsEnv = "YES" savedToolIdentifier = "" useCustomWorkingDirectory = "NO" debugDocumentVersioning = "YES"></ProfileAction>
  <AnalyzeAction buildConfiguration = "Debug"></AnalyzeAction>
  <ArchiveAction buildConfiguration = "Release" revealArchiveInOrganizer = "YES"></ArchiveAction>
</Scheme>
"""


if __name__ == "__main__":
    root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
    dest = os.path.join(root, PROJECT + ".xcodeproj")
    os.makedirs(os.path.join(dest, "xcshareddata", "xcschemes"), exist_ok=True)
    open(os.path.join(dest, "project.pbxproj"), "w").write(build(SPEC))
    open(os.path.join(dest, "xcshareddata", "xcschemes", PROJECT + ".xcscheme"), "w").write(scheme())
    print("wrote", dest)
