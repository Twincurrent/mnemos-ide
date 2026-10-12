#import "host_view.h"

#import <Metal/Metal.h>

void mnemos_view_did_change_backing_properties(MnemosHostView *, BOOL);
void mnemos_view_set_frame_size(MnemosHostView *, NSSize, BOOL);
void mnemos_update_layer(MnemosHostView *);
BOOL mnemos_handle_view_event(MnemosHostView *, NSEvent *, BOOL);
BOOL mnemos_handle_first_mouse_event(MnemosHostView *, NSEvent *);
void mnemos_handle_insert_text(MnemosHostView *, id);
void mnemos_update_ime_state(MnemosHostView *, BOOL);
void mnemos_handle_drag_and_drop(MnemosHostView *, NSArray *, NSPoint);
void mnemos_handle_file_drag(MnemosHostView *, NSPoint);
void mnemos_handle_file_drag_exit(MnemosHostView *);
NSRect mnemos_ime_position(MnemosHostView *, NSRect *);
id mnemos_get_accessibility_contents(MnemosHostView *);
void mnemos_marked_text_updated(MnemosHostView *, NSString *, NSRange);
void mnemos_marked_text_cleared(MnemosHostView *);

@implementation NSPasteboard (Mnemos)

- (NSArray *)getFilePaths {
    NSMutableArray *paths = [NSMutableArray array];
    NSArray<NSURL *> *urls = [self readObjectsForClasses:@[ [NSURL class] ] options:0];
    for (NSURL *url in urls) {
        NSString *path = url.path;
        if (path) {
            [paths addObject:path];
        }
    }
    return paths;
}

@end

@implementation MnemosHostView {
    // The windowState is managed on the Rust side.
    // Note Rust expects this name even though we are not a window.
    void *windowState;

    // Whether we start a window drag on an unhandled mouseDown event inside the title bar
    BOOL titlebarDragEnabled;

    // Whether we are in test mode, which suppresses drawing.
    BOOL testMode;

    // The metal device for our layer.
    id metalDevice;

    NSMutableAttributedString *markedText;
    NSMutableString *textToInsert;

    // Whether to have resize event callback called asynchronously.
    BOOL asyncCallback;

    // Whether we're in the middle of a call to interpretKeyEvents.
    BOOL interpretingKeyEvents;
}

- (BOOL)acceptsFirstResponder {
    return YES;
}

- (BOOL)mouseDownCanMoveWindow {
    return !titlebarDragEnabled;
}

- (BOOL)readyForMnemos {
    return windowState != NULL;
}

/// Returns the height of the titlebar.
- (CGFloat)titlebarHeight {
    NSButton *closeButton = [self.window standardWindowButton:NSWindowCloseButton];
    NSView *titlebar = [closeButton superview];
    return titlebar.frame.size.height;
}

- (BOOL)mouseInTitleBar:(NSEvent *)event {
    NSPoint windowLoc = [self convertPoint:event.locationInWindow fromView:nil];
    // windowLoc.y is the distance from the bottom of the window to the cursor
    // NSHeight(window.frame) will be the height of the whole window, so
    // NSHeight - titlebarHeight will be the bottom border of the titlebar
    return NSHeight(self.window.frame) - [self titlebarHeight] <= windowLoc.y;
}

// See if the user double clicked in the titlebar. If so, do whatever
// action is given by preferences.
// \return true if handled, false otherwise.
- (BOOL)handleTitleBarDoubleClick:(NSEvent *)event {
    NSWindow *window = self.window;
    NSWindowStyleMask styleMask = window.styleMask;
    // Was this a double click in a full-sized content view, not in full screen?
    if (event.clickCount != 2) return NO;
    if (!(styleMask & NSWindowStyleMaskFullSizeContentView)) return NO;
    if (styleMask & NSWindowStyleMaskFullScreen) return NO;

    // See if our point is in the titlebar of the window.
    if (![self mouseInTitleBar:event]) return NO;

    // Ok, do the action.
    NSString *action =
        [[NSUserDefaults standardUserDefaults] objectForKey:@"AppleActionOnDoubleClick"];

    // When user has not explicitly ticked or unticked the `Double-click the window's
    // title bar to` option in system preferences, the NSUserDefaults will not have the key
    // "AppleActionOnDoubleClick", despite in system preferences the default is to "Zoom".
    // To make the behavior consistent, when the key is nil, we set performZoom as the
    // default behavior here.
    if ([action isEqualToString:@"Minimize"]) {
        [window performMiniaturize:nil];
        return YES;
    } else if (action == nil || [action isEqualToString:@"Maximize"]) {
        [window performZoom:nil];
        return YES;
    }
    return NO;
}

- (void)viewDidChangeBackingProperties {
    if (self.readyForMnemos) mnemos_view_did_change_backing_properties(self, asyncCallback);
    [super viewDidChangeBackingProperties];
}

- (void)setFrameSize:(NSSize)size {
    BOOL changed = !NSEqualSizes(size, self.frame.size);
    // We could receive invalid frame sizes when the window is moved offscreen.
    // Validate the size against the minimum drawable size of the window before
    // passing to the rust side.
    if (size.height >= self.window.minSize.height && size.width >= self.window.minSize.width) {
        [super setFrameSize:size];
        // It's an important optimization to only invoke this if the size changed.
        if (self.readyForMnemos && changed) {
            mnemos_view_set_frame_size(self, size, asyncCallback);
        }
    }
}

- (void)displayLayer:(CALayer *)layer {
    if (!testMode && self.readyForMnemos) {
        mnemos_update_layer(self);
    }
}

- (void)setAsyncCallback:(BOOL)shouldAsync {
    asyncCallback = shouldAsync;
}

- (void)keyDown:(NSEvent *)event {
    [self keyDownImpl:event];
}

- (BOOL)keyDownImpl:(NSEvent *)event {
    BOOL wasComposing = [self hasMarkedText];
    [textToInsert setString:@""];

    // Interpret the key events here so we could check whether user is composing
    // text within the IME and pass the state down to the KeyDown events.
    interpretingKeyEvents = YES;
    [self interpretKeyEvents:[NSArray arrayWithObject:event]];
    interpretingKeyEvents = NO;

    BOOL handled = NO;
    if (self.readyForMnemos) {
        handled = mnemos_handle_view_event(self, event, wasComposing || [self hasMarkedText]);
    }

    // It's possible to have keybinding conflicts between terminal apps which use the meta key and
    // MacOS "dead keys". Dead keys are used to add diacritical marks to other characters, and they
    // start composing marked text. To detect if a keybinding was triggered in the app, `handled`
    // will be true. If that is the case, we don't want MacOS to also start composing because we
    // already handled that keydown elsewhere. So, if `justStartedComposing` is also true, clear
    // out the marked text.
    // https://support.apple.com/guide/mac-help/enter-characters-with-accent-marks-on-mac-mh27474/mac#mchl45cdda7f
    BOOL justStartedComposing = !wasComposing && [self hasMarkedText];
    if (handled && justStartedComposing) {
        NSTextInputContext *inputContext = [self inputContext];
        [inputContext discardMarkedText];
        [self unmarkText];
    }

    // Dispatch TypedCharacter event after KeyDown has been dispatched.
    if ([textToInsert length] > 0 && !handled) {
        mnemos_handle_insert_text(self, (NSString *)textToInsert);
        [self unmarkText];
    }

    return handled;
}

- (BOOL)acceptsFirstMouse:(NSEvent *)event {
    // We want to receive mouseDown events even if the window is not key
    // and we explicity fire the event here so that Mnemos can handle it.
    if (self.readyForMnemos) mnemos_handle_first_mouse_event(self, event);

    // We return NO though so that the event is not fired twice (returning YES
    // would result in the event being passed to the mouseDown handler).
    return NO;
}

- (void)mouseDown:(NSEvent *)event {
    if (self.readyForMnemos) {
        BOOL eventHandled = mnemos_handle_view_event(self, event, NO);
        if (self->titlebarDragEnabled && !eventHandled && [self mouseInTitleBar:event]) {
            // If Mnemos doesn't do anything with the event, indicated by returning `false`, and
            // if the drag starts in the titlebar, begin dragging the window
            [self.window performWindowDragWithEvent:event];
        }
    }
}

- (void)mouseUp:(NSEvent *)event {
    // Our content view is full-size so we don't get the default behavior
    // on titlebar clicks. Implement it manually.
    BOOL mnemos_handled = NO;
    if (self.readyForMnemos) {
        mnemos_handled = mnemos_handle_view_event(self, event, NO);
    }
    if (!mnemos_handled) {
        [self handleTitleBarDoubleClick:event];
    }
}

- (void)otherMouseDown:(NSEvent *)event {
    if (self.readyForMnemos) mnemos_handle_view_event(self, event, NO);
}

- (void)rightMouseDown:(NSEvent *)event {
    if (self.readyForMnemos) mnemos_handle_view_event(self, event, NO);
}

- (void)mouseDragged:(NSEvent *)event {
    if (self.readyForMnemos) mnemos_handle_view_event(self, event, NO);
}

- (void)scrollWheel:(NSEvent *)event {
    if (self.readyForMnemos) mnemos_handle_view_event(self, event, NO);
}

- (void)mouseMoved:(NSEvent *)event {
    if (self.readyForMnemos) mnemos_handle_view_event(self, event, NO);
}

- (void)flagsChanged:(NSEvent *)event {
    if (self.readyForMnemos) mnemos_handle_view_event(self, event, NO);
}

- (void)dealloc {
    [markedText release];
    [textToInsert release];
    [metalDevice release];
    [super dealloc];
}

- (CALayer *)makeBackingLayer {
    CAMetalLayer *layer = [CAMetalLayer layer];
    layer.pixelFormat = MTLPixelFormatBGRA8Unorm;
    layer.device = metalDevice;
    layer.allowsNextDrawableTimeout = NO;
    layer.autoresizingMask = kCALayerWidthSizable | kCALayerHeightSizable;
    layer.needsDisplayOnBoundsChange = YES;
    layer.presentsWithTransaction = YES;
    layer.delegate = self;
    layer.opaque = NO;
    return layer;
}

- (MnemosHostView *)initWithFrame:(NSRect)frame
                    metalDevice:(id)device
             enableTitlebarDrag:(BOOL)enableTitlebarDrag
                       testMode:(BOOL)testModeFlag {
    NSAssert(testModeFlag || device, @"Nil metal device not in test mode");
    [super initWithFrame:frame];

    // Register here so we could receive drag and drop events.
    [self registerForDraggedTypes:@[
        NSPasteboardTypeFileURL,
    ]];
    self->testMode = testModeFlag;
    self->titlebarDragEnabled = enableTitlebarDrag;
    self->metalDevice = [device retain];
    self->markedText = [[NSMutableAttributedString alloc] init];
    self->textToInsert = [[NSMutableString alloc] init];
    self->asyncCallback = YES;
    self.autoresizingMask = NSViewWidthSizable | NSViewHeightSizable;
    self.wantsLayer = YES;
    self.layerContentsRedrawPolicy = NSViewLayerContentsRedrawDuringViewResize;
    return self;
}

// Entry point for drag & drop. Check whether the source is an acceptable type and if so
// pass it down to performDragOperaion.
- (NSDragOperation)draggingEntered:(id<NSDraggingInfo>)sender {
    NSDragOperation sourceMask = [sender draggingSourceOperationMask];

    BOOL pasteOK =
        !![[sender draggingPasteboard] availableTypeFromArray:@[ NSPasteboardTypeFileURL ]];
    if (pasteOK && (sourceMask & NSDragOperationCopy)) {
        return NSDragOperationCopy;
    }
    return NSDragOperationNone;
}

// Called continuously while the drag operation is occurring within the view
- (NSDragOperation)draggingUpdated:(id<NSDraggingInfo>)sender {
    NSPoint dragPoint = [sender draggingLocation];
    NSPoint localPoint = [self convertPoint:dragPoint fromView:nil];

    NSPasteboard *pasteboard = [sender draggingPasteboard];
    if (self.readyForMnemos) {
        NSArray *types = [pasteboard types];
        if ([types containsObject:NSPasteboardTypeFileURL]) {
            mnemos_handle_file_drag(self, localPoint);
            return YES;
        }
    }
    return NSDragOperationNone;
}

- (void)draggingExited:(id<NSDraggingInfo>)sender {
    if (self.readyForMnemos) {
        mnemos_handle_file_drag_exit(self);
    }
}

- (BOOL)performDragOperation:(id<NSDraggingInfo>)sender {
    NSPasteboard *pasteboard = [sender draggingPasteboard];
    NSDragOperation dragOperation = [sender draggingSourceOperationMask];

    NSPoint dragPoint = [sender draggingLocation];
    NSPoint localPoint = [self convertPoint:dragPoint fromView:nil];

    if (self.readyForMnemos && (dragOperation & NSDragOperationCopy)) {
        NSArray *types = [pasteboard types];
        if ([types containsObject:NSPasteboardTypeFileURL]) {
            mnemos_handle_drag_and_drop(self, [pasteboard getFilePaths], localPoint);
            return YES;
        }
    }
    return NO;
}

- (void)closeIMEAsync {
    dispatch_async(dispatch_get_main_queue(), ^{
      NSTextInputContext *inputContext = [self inputContext];
      [inputContext discardMarkedText];

      [self unmarkText];
    });
}

#pragma mark - Accessibility
- (BOOL)isAccessibilityElement {
    return YES;
}

- (NSAccessibilityRole)accessibilityRole {
    return NSAccessibilityTextAreaRole;
}

- (NSString *)accessibilityRoleDescription {
    return NSAccessibilityRoleDescriptionForUIElement(self);
}

- (BOOL)isAccessibilityFocused {
    return YES;
}

- (id)accessibilityValue {
    return mnemos_get_accessibility_contents(self);
}

- (NSInteger)accessibilityNumberOfCharacters {
    return 0;
}

- (NSInteger)accessibilityInsertionPointLineNumber {
    return 0;
}

- (NSString *)accessibilityDocument {
    return nil;
}

////////////////////////////////////////////////////////////////////////////////
// NSTextInputClient protocol implementation
////////////////////////////////////////////////////////////////////////////////

- (nullable NSAttributedString *)attributedSubstringForProposedRange:(NSRange)range
                                                         actualRange:
                                                             (nullable NSRangePointer)actualRange {
    return nil;
}

- (NSUInteger)characterIndexForPoint:(NSPoint)thePoint {
    return (NSUInteger)0;
}

// This is a no-op as we will be handling control characters in KeyDown events.
- (void)doCommandBySelector:(SEL)selector {
}

- (NSRect)firstRectForCharacterRange:(NSRange)range
                         actualRange:(nullable NSRangePointer)actualRange {
    NSWindow *window = self.window;
    if (self.readyForMnemos) {
        NSRect contentRect = [window contentRectForFrameRect:[window frame]];
        NSRect rect = mnemos_ime_position(self, &contentRect);
        return rect;
    } else {
        return NSZeroRect;
    }
}

- (BOOL)hasMarkedText {
    return [markedText length] > 0;
}

// Referenced glfw for this implementation.
// https://github.com/glfw/glfw/blob/7ef34eb06de54dd9186d3d21a401b2ef819b59e7/src/cocoa_window.m#L814
- (void)insertText:(id)string replacementRange:(NSRange)replacementRange {
    if (self.readyForMnemos) {
        NSMutableString *characters = [[NSMutableString alloc] init];

        if ([string isKindOfClass:[NSAttributedString class]]) {
            // We are appending rather than replacing here because sometimes insertText
            // could be fired multiple times in a row. For example, when user types
            // Option-E followed by g, insertText will fire ´ first and then g.
            [characters appendString:[string string]];
        } else {
            [characters appendString:(NSString *)string];
        }

        // If we're in the middle of a call to interpretKeyEvents, batch up all
        // inserted text, as we may handle the event during `keyDown`.  If this
        // call to `insertText` is not in a call stack underneath `keyDown`
        // (e.g.: when inserting an emoji from the emoji composer), just insert
        // the text directly.
        if (interpretingKeyEvents) {
            [textToInsert appendString:characters];
        } else {
            mnemos_handle_insert_text(self, (NSString *)characters);
        }

        [characters release];
    }
    // When handling the key down Enter, we might need to rely on the IME being open
    // to accept the marked text as-is and so can't call unmarkText.
    if (!interpretingKeyEvents) {
        [self unmarkText];
    }
}

- (NSRange)markedRange {
    if ([markedText length] > 0)
        return NSMakeRange(0, [markedText length]);
    else
        return NSMakeRange(NSNotFound, 0);
}

- (NSRange)selectedRange {
    return NSMakeRange(0, 0);
}

- (void)setMarkedText:(id)string
        selectedRange:(NSRange)selectedRange
     replacementRange:(NSRange)replacementRange {
    [markedText release];
    if ([string isKindOfClass:[NSAttributedString class]])
        markedText = [[NSMutableAttributedString alloc] initWithAttributedString:string];
    else
        markedText = [[NSMutableAttributedString alloc] initWithString:string];

    if (self.readyForMnemos) {
        mnemos_marked_text_updated(self, markedText.string, selectedRange);
        if ([markedText length] > 0) {
            mnemos_update_ime_state(self, YES);
        } else {
            mnemos_update_ime_state(self, NO);
        }
    }
}

- (void)unmarkText {
    [[markedText mutableString] setString:@""];
    if (self.readyForMnemos) {
        mnemos_update_ime_state(self, NO);
        mnemos_marked_text_cleared(self);
    }
}

- (NSArray<NSString *> *)validAttributesForMarkedText {
    return [NSArray array];
}

@end
