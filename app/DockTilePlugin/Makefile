BUNDLE_NAME = MnemosDockTilePlugin.docktileplugin
OBJC_FILES = MnemosDockTilePlugin.m
FRAMEWORKS = -framework Cocoa -framework AppKit -framework Foundation
# Compile a universal binary for both arm64 and x86_64.
CFLAGS = -fobjc-arc -bundle -mmacosx-version-min=$(MACOSX_DEPLOYMENT_TARGET) -arch arm64 -arch x86_64
LDFLAGS = -ObjC

.PHONY: all clean resources

all: $(BUNDLE_NAME)/Contents/MacOS/MnemosDockTilePlugin resources

$(BUNDLE_NAME)/Contents/MacOS/MnemosDockTilePlugin: $(OBJC_FILES) MnemosDockTilePlugin.h
	mkdir -p $(BUNDLE_NAME)/Contents/MacOS
	clang $(CFLAGS) $(LDFLAGS) $(FRAMEWORKS) $(OBJC_FILES) -o $@

# Always resync the plist and icons, so an icon deleted from Resources/ never survives in
# an existing build directory.
resources:
	mkdir -p $(BUNDLE_NAME)/Contents
	cp Info.plist $(BUNDLE_NAME)/Contents/
	rm -rf $(BUNDLE_NAME)/Contents/Resources
	mkdir -p $(BUNDLE_NAME)/Contents/Resources
	cp Resources/* $(BUNDLE_NAME)/Contents/Resources/

clean:
	rm -rf $(BUNDLE_NAME)
