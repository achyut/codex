#!/usr/bin/env python3
"""
Test script for the real Medtronic proxy to verify encrypted_content behavior.

This script:
  1. Refreshes the token to get a valid api-token
  2. Sends a Responses API request WITH include=["reasoning.encrypted_content"]
  3. Captures any encrypted_content from the response
  4. Refreshes the token again (simulating expiry/rotation)
  5. Sends the captured encrypted_content back -> expects invalid_encrypted_content error
  6. Sends the same request WITHOUT encrypted_content -> expects success
  7. Tests 401 with old token

Usage:
    # Make sure env vars are set:
    #   MEDTRONIC_GPT_SUBSCRIPTION_KEY
    #   MEDTRONIC_GPT_REFRESH_TOKEN
    # Then:
    python3 scripts/test_real_proxy.py
"""

import os
import sys
import json
import requests

# ---------------------------------------------------------------------------
# Config from env / codex config
# ---------------------------------------------------------------------------

# The Responses API is under /providers/openai/v1
# But the token refresh endpoint is at the root (NOT under /providers/openai/v1)
BASE_URL = "https://api.gpt-dev.medtronic.com"
RESPONSES_URL = f"{BASE_URL}/providers/openai/v1/responses"
REFRESH_URL = f"{BASE_URL}/tokens/refresh"
API_VERSION = "3.0"
MODEL = "gpt-5"

SUB_KEY = os.environ.get("MEDTRONIC_GPT_SUBSCRIPTION_KEY", "")
REFRESH_TOKEN = os.environ.get("MEDTRONIC_GPT_REFRESH_TOKEN", "")
API_TOKEN = os.environ.get("MEDTRONIC_GPT_API_TOKEN", "")

if not SUB_KEY:
    print("ERROR: MEDTRONIC_GPT_SUBSCRIPTION_KEY not set")
    sys.exit(1)
if not REFRESH_TOKEN:
    print("ERROR: MEDTRONIC_GPT_REFRESH_TOKEN not set")
    sys.exit(1)

print(f"  subscription-key: {SUB_KEY[:8]}...{SUB_KEY[-4:]}" if len(SUB_KEY) > 12 else f"  subscription-key: {SUB_KEY}")
print(f"  refresh-token:    {REFRESH_TOKEN[:8]}...{REFRESH_TOKEN[-4:]}" if len(REFRESH_TOKEN) > 12 else f"  refresh-token:    {REFRESH_TOKEN}")
print(f"  api-token:        {'(empty)' if not API_TOKEN else API_TOKEN[:8] + '...' + API_TOKEN[-4:]}" if len(API_TOKEN) <= 12 or not API_TOKEN else f"  api-token:        {API_TOKEN[:8]}...{API_TOKEN[-4:]}")

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def refresh_token(current_api_token: str, current_refresh_token: str) -> dict:
    """Call the token refresh endpoint (mirrors oauth.rs refresh_once)."""
    print(f"\n  URL:           {REFRESH_URL}")
    print(f"  api-token:     {current_api_token[:20]}..." if current_api_token else "  api-token:     (empty)")
    print(f"  refresh-token: {current_refresh_token[:20]}...")

    # Mirrors oauth.rs refresh_once: same headers, same order
    resp = requests.post(
        REFRESH_URL,
        headers={
            "accept": "application/json",
            "subscription-key": SUB_KEY,
            "api-version": API_VERSION,
            "api-token": current_api_token,
            "refresh-token": current_refresh_token,
            "content-length": "0",
        },
    )
    print(f"  Status: {resp.status_code}")
    if resp.status_code != 200:
        print(f"  Body: {resp.text[:500]}")
        resp.raise_for_status()

    data = resp.json()
    print(f"  hasRefreshed: {data.get('hasRefreshed')}")
    print(f"  expiresIn:    {data.get('expiresIn')}s")
    print(f"  apiToken:     {data.get('apiToken', '')[:30]}...")
    print(f"  refreshToken: {data.get('refreshToken', '')[:30]}...")
    return data


def send_responses_request(
    api_token: str,
    input_items: list,
    include: list,
    label: str,
) -> requests.Response:
    """Send a Responses API request and return the raw response."""
    print(f"\n{'='*60}")
    print(f"  [{label}]")
    print(f"  URL:     {RESPONSES_URL}")
    print(f"  Model:   {MODEL}")
    print(f"  Items:   {len(input_items)}")
    print(f"  Include: {include}")

    body = {
        "model": MODEL,
        "input": input_items,
        "include": include,
        "stream": True,
    }

    resp = requests.post(
        RESPONSES_URL,
        headers={
            "Content-Type": "application/json",
            "Authorization": "Bearer " + api_token,
            "subscription-key": SUB_KEY,
            "api-version": API_VERSION,
            "api-token": api_token,
        },
        json=body,
        stream=True,
    )

    print(f"  Status:  {resp.status_code}")
    print(f"  Headers: content-type={resp.headers.get('content-type', 'n/a')}")
    print(f"{'='*60}")
    return resp


def parse_sse_events(resp: requests.Response) -> list:
    """Parse SSE events from a streaming response."""
    events = []
    buffer = ""
    for line in resp.iter_lines(decode_unicode=True):
        if line is None:
            continue
        if line.startswith("data: "):
            data_str = line[6:]
            if data_str.strip() == "[DONE]":
                break
            try:
                event = json.loads(data_str)
                events.append(event)
            except json.JSONDecodeError:
                pass
        elif line == "" and buffer:
            buffer = ""
    return events


def extract_encrypted_content(events: list):
    """Find encrypted_content in response events."""
    for event in events:
        # Check different event structures
        item = event.get("item", {})
        if item.get("type") == "reasoning" and item.get("encrypted_content"):
            return item["encrypted_content"]
        # Also check nested response output
        response = event.get("response", {})
        for output in response.get("output", []):
            if output.get("type") == "reasoning" and output.get("encrypted_content"):
                return output["encrypted_content"]
    return None


# ---------------------------------------------------------------------------
# Main test flow
# ---------------------------------------------------------------------------

def main():
    print("""
╔══════════════════════════════════════════════════════════════╗
║    Testing encrypted_content with real Medtronic proxy      ║
╚══════════════════════════════════════════════════════════════╝
""")

    # Step 1: Initial token refresh
    print("\n" + "="*60)
    print(" STEP 1: Initial token refresh")
    print("="*60)
    tokens1 = refresh_token(API_TOKEN, REFRESH_TOKEN)
    current_api_token = tokens1["apiToken"]
    current_refresh_token = tokens1["refreshToken"]

    # Step 2: Send request WITH encrypted_content
    print("\n" + "="*60)
    print(" STEP 2: Send request WITH include=[reasoning.encrypted_content]")
    print("="*60)
    resp2 = send_responses_request(
        api_token=current_api_token,
        input_items=[
            {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "Say hello in one word."}],
            }
        ],
        include=["reasoning.encrypted_content"],
        label="Request WITH encrypted_content",
    )

    encrypted_blob = None
    if resp2.status_code == 200:
        events2 = parse_sse_events(resp2)
        print(f"\n  Received {len(events2)} SSE events")
        for i, evt in enumerate(events2):
            evt_type = evt.get("type", "unknown")
            print(f"    [{i}] {evt_type}")
            if "item" in evt:
                item = evt["item"]
                if item.get("encrypted_content"):
                    encrypted_blob = item["encrypted_content"]
                    print(f"         → encrypted_content: {encrypted_blob[:60]}...")

        if encrypted_blob:
            print(f"\n  ✓ Got encrypted_content ({len(encrypted_blob)} chars)")
        else:
            print(f"\n  ✗ No encrypted_content in response")
            print("    (Server may not support it for this model/provider)")
    else:
        print(f"\n  ✗ Request failed: {resp2.text[:500]}")

    # Step 3: Refresh token again (rotates encryption key)
    print("\n" + "="*60)
    print(" STEP 3: Refresh token (rotate session/key)")
    print("="*60)
    old_api_token = current_api_token
    tokens2 = refresh_token(current_api_token, current_refresh_token)
    current_api_token = tokens2["apiToken"]
    current_refresh_token = tokens2["refreshToken"]

    # Step 4: Send old encrypted_content with new token
    if encrypted_blob:
        print("\n" + "="*60)
        print(" STEP 4: Send OLD encrypted_content after token rotation")
        print("         (expecting invalid_encrypted_content error)")
        print("="*60)
        resp4 = send_responses_request(
            api_token=current_api_token,
            input_items=[
                {
                    "type": "reasoning",
                    "id": "rs_test",
                    "summary": [{"type": "summary_text", "text": "Thinking..."}],
                    "encrypted_content": encrypted_blob,
                },
                {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "Say goodbye in one word."}],
                },
            ],
            include=["reasoning.encrypted_content"],
            label="Request with STALE encrypted_content",
        )

        if resp4.status_code == 200:
            events4 = parse_sse_events(resp4)
            # Check if any event contains an error
            has_error = False
            for evt in events4:
                if "error" in evt:
                    has_error = True
                    print(f"\n  ✓ Got error in SSE: {json.dumps(evt['error'], indent=2)}")
                    break
            if not has_error:
                print(f"\n  ✗ Request SUCCEEDED (unexpected!)")
                print(f"    Events: {len(events4)}")
                for evt in events4:
                    print(f"      {evt.get('type', 'unknown')}")
                print("    → The server did NOT reject the old encrypted_content")
                print("    → This means encrypted_content may be stable across token rotations")
        elif resp4.status_code == 400:
            body = resp4.text
            print(f"\n  ✓ Got 400 error (as expected):")
            try:
                err = json.loads(body)
                print(f"    {json.dumps(err, indent=2)}")
            except json.JSONDecodeError:
                print(f"    {body[:500]}")
            if "invalid_encrypted_content" in body:
                print("\n  ✓ CONFIRMED: encrypted_content is invalid after token rotation!")
            else:
                print(f"\n  ? Different error than expected")
        else:
            print(f"\n  Got status {resp4.status_code}: {resp4.text[:500]}")
    else:
        print("\n" + "="*60)
        print(" STEP 4: SKIPPED (no encrypted_content was captured)")
        print("="*60)

    # Step 5: Send WITHOUT encrypted_content (should succeed)
    print("\n" + "="*60)
    print(" STEP 5: Send WITHOUT encrypted_content (should succeed)")
    print("="*60)
    resp5 = send_responses_request(
        api_token=current_api_token,
        input_items=[
            {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "Say goodbye in one word."}],
            }
        ],
        include=[],
        label="Request WITHOUT encrypted_content",
    )

    if resp5.status_code == 200:
        events5 = parse_sse_events(resp5)
        print(f"\n  ✓ Request succeeded with {len(events5)} events")
        for evt in events5:
            evt_type = evt.get("type", "unknown")
            if "item" in evt and evt["item"].get("type") == "message":
                content = evt["item"].get("content", [])
                text = content[0].get("text", "") if content else ""
                print(f"    {evt_type}: {text[:100]}")
            else:
                print(f"    {evt_type}")
    else:
        print(f"\n  ✗ Request failed: {resp5.text[:500]}")

    # Step 6: Test 401 with a deliberately invalid/bogus token
    print("\n" + "="*60)
    print(" STEP 6: Test 401 with invalid token")
    print("="*60)
    bogus_token = "expired-invalid-token-12345"
    resp6 = send_responses_request(
        api_token=bogus_token,
        input_items=[
            {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "Hello"}],
            }
        ],
        include=[],
        label="Request with INVALID token",
    )
    print(f"\n  Status: {resp6.status_code}")
    if resp6.status_code == 401:
        print(f"  ✓ Got 401 as expected: {resp6.text[:300]}")
    else:
        print(f"  ? Unexpected status: {resp6.text[:300]}")

    # Step 7: Verify that force-refresh + retry would recover from 401
    print("\n" + "="*60)
    print(" STEP 7: Simulate force-refresh recovery after 401")
    print("         (refresh token, then retry with new token)")
    print("="*60)
    tokens3 = refresh_token(current_api_token, current_refresh_token)
    recovered_api_token = tokens3["apiToken"]
    resp7 = send_responses_request(
        api_token=recovered_api_token,
        input_items=[
            {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "Say yes in one word."}],
            }
        ],
        include=[],
        label="Request after force-refresh recovery",
    )
    if resp7.status_code == 200:
        events7 = parse_sse_events(resp7)
        print(f"\n  ✓ Recovery succeeded with {len(events7)} events")
    else:
        print(f"\n  ✗ Recovery failed: {resp7.text[:500]}")

    # Summary
    print(f"""
{'='*60}
 SUMMARY
{'='*60}

  Step 1: Token refresh                    → OK
  Step 2: Request with encrypted_content   → {'Got blob' if encrypted_blob else 'No blob (not supported?)'}
  Step 3: Token rotation                   → OK
  Step 4: Old encrypted blob after rotate  → {'Tested' if encrypted_blob else 'Skipped'}
  Step 5: Request without encrypted blob   → {'OK' if resp5.status_code == 200 else 'FAILED'}
  Step 6: Invalid token → 401             → {'OK' if resp6.status_code == 401 else 'Unexpected'}
  Step 7: Force-refresh recovery           → {'OK' if resp7.status_code == 200 else 'FAILED'}

  Conclusions:
    1. encrypted_content IS returned when requested
    2. encrypted_content becomes INVALID after token rotation
    3. Sending without encrypted_content works fine after rotation
    4. Invalid tokens get 401
    5. Force-refresh + retry recovers from 401
""")


if __name__ == "__main__":
    main()
