import time
import sys
from playwright.sync_api import sync_playwright

def run():
    print("Launching Playwright to test http://localhost:5173...")
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True)
        context = browser.new_context()
        page = context.new_page()
        
        # Go to landing page
        page.goto('http://localhost:5173')
        page.wait_for_load_state('networkidle')
        
        # Verify page title
        title = page.title()
        print(f"Page Title: {title}")
        assert "Beacon & Pulse" in title, f"Unexpected title: {title}"
        
        # Verify Hero title text
        hero_title = page.locator("h1").inner_text()
        print(f"Hero Title: {hero_title}")
        assert "BEACON & PULSE" in hero_title, "Hero title missing"
        
        # Focus the Pulse Client window first to bring it to the front
        print("Clicking Pulse Player shortcut to focus the window...")
        page.locator('div[data-open-win="win-pulse"]').click()
        time.sleep(0.5)

        # Locate simulator elements
        btn_connect = page.locator("#sim-btn-connect")
        assert btn_connect.is_visible(), "Connect button not visible initially"
        
        # 1. Click "Scan LAN for Hosts"
        print("Clicking 'Scan LAN for Hosts'...")
        btn_connect.click()
        
        # Wait for the host item to appear (it shows up after progress starts scanning)
        print("Waiting for host discovery...")
        host_item = page.locator("#host-item-1")
        host_item.wait_for(state="visible", timeout=10000)
        print("Host item discovered!")
        
        # 2. Click on the discovered host
        print("Clicking discovered host item...")
        host_item.click()
        
        # Wait for code input fields
        inputs = page.locator(".sim-code-input")
        inputs.first.wait_for(state="visible", timeout=2000)
        assert inputs.count() == 6, f"Expected 6 code input fields, found {inputs.count()}"
        
        # 3. Enter pairing code "582914"
        print("Entering pairing code 582914...")
        code = "582914"
        for i in range(6):
            inputs.nth(i).fill(code[i])
            # Small delay to trigger input events
            time.sleep(0.1)
            
        # 4. Click "Verify & Connect"
        btn_verify = page.locator("#sim-btn-verify")
        print("Clicking Verify & Connect...")
        btn_verify.click()
        
        # Verify transition to connected state
        view_connected = page.locator("#sim-view-connected")
        view_connected.wait_for(state="visible", timeout=3000)
        print("Successfully transitioned to Connected state!")
        
        # 5. Take screenshot of the connected mockup remote desktop
        screenshot_path = "c:/Users/JAISINGH/.gemini/antigravity/brain/e460db73-14d9-4d86-b443-6e40737ced30/scratch/connected_simulator.png"
        page.screenshot(path=screenshot_path, full_page=False)
        print(f"Saved simulator screenshot to {screenshot_path}")
        
        # 6. Click Clipboard Sync
        print("Testing Clipboard Sync (triggering action)...")
        btn_sync = page.locator("#sim-btn-sync-clipboard")
        
        # Mock the window.prompt to automatically return text when prompted
        page.evaluate("window.prompt = () => 'Auto testing clipboard sync';")
        btn_sync.click()
        
        # Check logs on host terminal on the right side
        logs_locator = page.locator("#host-terminal-logs")
        logs_locator.wait_for(state="visible", timeout=2000)
        time.sleep(1.0) # Let the typing animation complete
        logs_content = logs_locator.inner_text()
        print("Checking Host Console logs for Clipboard Sync...")
        assert "Received ControlMessage::ClipboardSync" in logs_content, "Clipboard sync not found in host logs"
        print("Clipboard Sync log entry verified!")
        
        # 7. Click File Transfer
        print("Testing File Transfer...")
        btn_send = page.locator("#sim-btn-send-file")
        btn_send.click()
        
        # Wait for upload simulation and check logs
        time.sleep(2.0)
        logs_content = logs_locator.inner_text()
        print("Checking Host Console logs for File Transfer...")
        assert "setup_archive.zip" in logs_content, "File transfer request not found in host logs"
        assert "integrity verification" in logs_content, "SHA-256 integrity log not found in host logs"
        print("File Transfer log entries verified!")
        
        # 8. Test CLI Command Playground
        print("Testing CLI playground controls...")
        cli_lbl_bitrate = page.locator("#cli-lbl-bitrate")
        assert cli_lbl_bitrate.inner_text() == "20", "Default bitrate should be 20 Mbps"
        
        cli_cmd_display = page.locator("#cli-cmd-display")
        cmd_text = cli_cmd_display.inner_text()
        print(f"Default command: {cmd_text}")
        assert "beacon.exe" in cmd_text, "Default CLI display command should be beacon host"
        
        # Click Player tab
        tab_player = page.locator("#cli-tab-player")
        tab_player.click()
        time.sleep(0.2)
        player_cmd_text = cli_cmd_display.inner_text()
        print(f"Player command: {player_cmd_text}")
        assert "pulse.exe" in player_cmd_text, "CLI display command should switch to pulse client"
        
        # 9. Disconnect session
        print("Refocusing Pulse Player window...")
        page.locator('div[data-open-win="win-pulse"]').click()
        time.sleep(0.5)

        print("Disconnecting simulator...")
        btn_disconnect = page.locator("#sim-btn-disconnect")
        btn_disconnect.click()
        
        # Verify back to offline state
        view_disconnected = page.locator("#sim-view-disconnected")
        view_disconnected.wait_for(state="visible", timeout=2000)
        print("Successfully disconnected back to landing state!")
        
        browser.close()
        print("Verification completed successfully!")

if __name__ == "__main__":
    run()
