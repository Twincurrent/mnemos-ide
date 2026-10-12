#import <AppKit/AppKit.h>
#import <QuartzCore/QuartzCore.h>

@interface NSPasteboard (Mnemos)
- (NSArray *)getFilePaths;
@end

/// MnemosHostView is the Content view of a Mnemos window.
// It is backed by a Metal CALayer.
@interface MnemosHostView : NSView <CALayerDelegate, NSTextInputClient>
- (MnemosHostView *)initWithFrame:(NSRect)frame
                    metalDevice:(id)metalDevice
             enableTitlebarDrag:(BOOL)enableTitlebarDrag
                       testMode:(BOOL)testMode;
- (void)setAsyncCallback:(BOOL)shouldAsync;
- (BOOL)keyDownImpl:(NSEvent *)event;
@end
