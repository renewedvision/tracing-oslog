#include "wrapper.h"

void wrapped_os_log_with_type(os_log_t log, os_log_type_t type, const char* message) {
    os_log_with_type(log, type, "%{public}s", message);
}
void wrapped_os_log_with_type_private(os_log_t log, os_log_type_t type, const char* public_message, const char* private_message) {
    os_log_with_type(log, type, "%{public}s %{private}s", public_message, private_message);
}
os_log_t wrapped_os_log_default(void) {
    return OS_LOG_DEFAULT;
}
