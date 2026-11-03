# Helpers for the Linux screenshot scripts under an X server
# (capture-release-screenshot.sh, capture-linux-web-screenshots.sh).
# Source it; it defines functions only.

# wait_for_openbox PID LOG: waits up to 30 seconds for the openbox at PID
# to take over the screen, and fails with its LOG if it exits or never
# does. A window the app maps while openbox is still starting can be lost:
# openbox never maps it, so the first state timed out with its window
# unmapped (#345). Openbox publishes _NET_NUMBER_OF_DESKTOPS late in its
# start-up, so once `xdotool get_num_desktops` answers it is managing new
# windows.
wait_for_openbox() {
  local pid="$1" log="$2"
  for _ in $(seq 1 150); do
    if ! kill -0 "${pid}" 2>/dev/null; then
      echo "openbox exited during start-up" >&2
      cat "${log}" >&2
      return 1
    fi
    xdotool get_num_desktops >/dev/null 2>&1 && return 0
    sleep 0.2
  done
  echo "timed out waiting for openbox to manage the screen" >&2
  cat "${log}" >&2
  return 1
}
