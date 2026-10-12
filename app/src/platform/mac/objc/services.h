// Our class for handling NSServices messages.
@interface MnemosServicesProvider : NSObject
@end

// Functions implemented in Rust.
id mnemos_services_provider_custom_url_scheme();
void mnemos_app_open_urls(id app, id urls);
