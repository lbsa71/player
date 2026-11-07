#ifndef MIXLIB_H
#define MIXLIB_H
#ifdef __cplusplus
extern "C" {
#endif
/* Call off the audio/main thread. Free the owned UTF-8 JSON response exactly once. */
char *mixlib_catalog_json(const char *library_path);
void mixlib_string_free(char *value);
#ifdef __cplusplus
}
#endif
#endif
