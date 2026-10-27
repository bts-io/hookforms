#!/usr/bin/env python3
"""
One-time OAuth2 authorization script for Gmail API.

Run this locally (NOT in Docker): it opens a browser for Google OAuth consent.
After authorizing, it saves a refresh token to config/gmail/token.json.

Usage:
    pip install google-auth-oauthlib
    python scripts/gmail_auth.py
"""

import json
from pathlib import Path

from google_auth_oauthlib.flow import InstalledAppFlow

SCOPES = [
    "https://www.googleapis.com/auth/gmail.send",
]

CREDENTIALS_PATH = Path(__file__).parent.parent / "config" / "gmail" / "credentials.json"
TOKEN_PATH = Path(__file__).parent.parent / "config" / "gmail" / "token.json"


def main():
    if not CREDENTIALS_PATH.exists():
        print(f"ERROR: {CREDENTIALS_PATH} not found.")
        print()
        print("To set up Gmail API credentials:")
        print("  1. Go to https://console.cloud.google.com/apis/credentials")
        print("  2. Create an OAuth 2.0 Client ID (Desktop app)")
        print("  3. Download the JSON and save it as config/gmail/credentials.json")
        return

    print("Starting OAuth2 authorization flow...")
    print("A browser window will open. Sign in and grant access.\n")

    flow = InstalledAppFlow.from_client_secrets_file(str(CREDENTIALS_PATH), SCOPES)
    creds = flow.run_local_server(port=8090, open_browser=True)

    token_data = {
        "token": creds.token,
        "refresh_token": creds.refresh_token,
        "token_uri": creds.token_uri,
        "client_id": creds.client_id,
        "client_secret": creds.client_secret,
        "scopes": list(creds.scopes),
    }

    TOKEN_PATH.parent.mkdir(parents=True, exist_ok=True)
    TOKEN_PATH.write_text(json.dumps(token_data, indent=2))
    print(f"\nToken saved to {TOKEN_PATH}")
    print("You can now start HookForms. It will use this token for Gmail access.")


if __name__ == "__main__":
    main()
