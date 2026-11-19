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
CONF_MDBLIST_KEY = "mdblist_api_key"
# The Criterion Channel account the TV's Criterion player signs in with, kept in the options.
CONF_CRITERION_EMAIL = "criterion_email"
CONF_CRITERION_PASSWORD = "criterion_password"
# A random fingerprint for the saved account, which TVs report back; never derived from it.
CONF_CRITERION_FINGERPRINT = "criterion_fingerprint"
# Fingerprints of accounts this server saved before, so a TV still holding one gets the new one.
CONF_CRITERION_EARLIER = "criterion_earlier"
# The fingerprint of an account cleared here, which TVs still holding it are told to forget.
CONF_CRITERION_CLEARED = "criterion_cleared"
# Phones (their mobile_app notify services) that offer to type into the TV's text fields.
CONF_KEYBOARD_PHONES = "keyboard_phones"
# Whether the TV keyboard text entity exists, so dashboards can type into the TV's fields.
CONF_KEYBOARD_ENTITY = "keyboard_entity"
# The webhook the TV calls when a text field opens or closes, kept in the entry's data.
CONF_WEBHOOK_ID = "webhook_id"

SERVICE_SHOW_CAMERA = "show_camera"
SERVICE_DISMISS = "dismiss"
SERVICE_NOTIFY = "notify"

POSITIONS = ["top-right", "top-left", "bottom-right", "bottom-left"]
SIZES = ["small", "medium", "large"]

# Developer access (off by default): cloud development sessions ask for a short-lived TV SSH key,
# approved on a phone. See dev_access.py. Options:
CONF_DEV_ACCESS = "dev_access"
CONF_DEV_PHONE = "dev_access_phone"
CONF_DEV_SSH_PORT = "dev_access_ssh_port"
CONF_DEV_TUNNEL = "dev_access_tunnel_hostname"
CONF_DEV_CF_ID = "dev_access_cloudflare_client_id"
CONF_DEV_CF_SECRET = "dev_access_cloudflare_client_secret"
# Entry data, created when developer access is first turned on:
CONF_DEV_TOKEN_HASH = "dev_access_token_hash"
CONF_DEV_KEY = "dev_access_management_key"
# Loaded only when developer access is on, so other installations never install it.
DEV_REQUIREMENTS = ["asyncssh==2.23.1"]
DEV_TOKEN_HEADER = "X-Luma-Dev-Token"
