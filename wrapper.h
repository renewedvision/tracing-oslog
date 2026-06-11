#include <os/log.h>
#include <os/activity.h>

void wrapped_os_log_with_type(os_log_t log, os_log_type_t type, const char *message);
void wrapped_os_log_with_type_private(os_log_t log, os_log_type_t type, const char *public_message, const char *private_message);
os_log_t wrapped_os_log_default(void);
