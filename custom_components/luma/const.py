"""Constants for the Luma integration."""

DOMAIN = "luma"
DEFAULT_PORT = 5862
# This integration's API level, sent with every request as INTEGRATION_HEADER. Raise it only when
# a TV must tell this integration apart from older ones; Luma names servers below its own minimum
# in Settings. Not the release version.
INTEGRATION_API = 1
INTEGRATION_HEADER = "X-Luma-Integration"
# The oldest TV API level (`api` in Luma's status) this integration works with.
MIN_TV_API = 1
CONF_TOKEN = "token"
CONF_DEVICE_ID = "device_id"
WEBOSTV_DOMAIN = "webostv"
# Streaming search keys, kept in the entry's options. Home Assistant is their source of truth.
CONF_MOTN_KEY = "motn_api_key"
CONF_TMDB_TOKEN = "tmdb_read_access_token"
# Phones (their mobile_app notify services) that offer to type into the TV's text fields.
CONF_KEYBOARD_PHONES = "keyboard_phones"
# The webhook the TV calls when a text field opens or closes, kept in the entry's data.
CONF_WEBHOOK_ID = "webhook_id"

SERVICE_SHOW_CAMERA = "show_camera"
SERVICE_DISMISS = "dismiss"
SERVICE_NOTIFY = "notify"

POSITIONS = ["top-right", "top-left", "bottom-right", "bottom-left"]
SIZES = ["small", "medium", "large"]
