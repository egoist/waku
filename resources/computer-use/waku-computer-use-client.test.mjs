import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { createSkyFacade } from "./waku-computer-use-client.mjs";

test("sky facade matches the Codex mac surface", async () => {
  const calls = [];
  const client = {
    async call(name, arguments_) {
      calls.push([name, arguments_]);
      if (name === "list_apps") {
        return {
          structuredContent: {
            apps: [{ id: "company.thebrowser.browser", displayName: "Helium", isRunning: true }],
          },
        };
      }
      if (name === "get_app_state") {
        return {
          structuredContent: {
            text: "[0] AXWindow \"Helium\"",
            screenshot: "data:image/png;base64,c2t5",
          },
        };
      }
      return { content: [] };
    },
  };
  const sky = createSkyFacade(client);

  assert.equal(sky.target, "mac");
  assert.deepEqual(await sky.list_apps(), [
    { id: "company.thebrowser.browser", displayName: "Helium", isRunning: true },
  ]);
  assert.deepEqual(
    await sky.get_app_state({ app: "company.thebrowser.browser", disableDiff: true }),
    {
      app: "company.thebrowser.browser",
      text: "[0] AXWindow \"Helium\"",
      screenshot: { url: "data:image/png;base64,c2t5" },
    },
  );
  await sky.select_text({
    app: "company.thebrowser.browser",
    element_index: 12,
    text: "browser",
    selection_type: "cursor_after",
  });

  assert.deepEqual(calls, [
    ["list_apps", {}],
    ["get_app_state", { app: "company.thebrowser.browser", disableDiff: true }],
    ["select_text", {
      app: "company.thebrowser.browser",
      element_index: 12,
      text: "browser",
      selection_type: "cursor_after",
    }],
  ]);
});

test("get_app_state preserves MCP image data as a data URL", async () => {
  const sky = createSkyFacade({
    async call() {
      return {
        content: [
          { type: "text", text: "tree" },
          { type: "image", data: "c2t5", mimeType: "image/png" },
        ],
      };
    },
  });

  assert.deepEqual(await sky.get_app_state({ app: "Notes" }), {
    app: "Notes",
    text: "tree",
    screenshot: { url: "data:image/png;base64,c2t5" },
  });
});

test("background drags use guarded Codex routing without foreground or global delivery", async () => {
  const source = await readFile(new URL("./WakuComputerUse.swift", import.meta.url), "utf8");
  const postStart = source.indexOf("private func post(_ event: CGEvent, to processID: pid_t)");
  const postEnd = source.indexOf("private func mouseButton", postStart);
  const dragStart = source.indexOf("private func drag(", postEnd);
  const dragEnd = source.indexOf("private func scroll(", dragStart);

  assert.ok(postStart >= 0 && postEnd > postStart, "targeted event post helper is missing");
  assert.ok(dragStart >= 0 && dragEnd > dragStart, "drag implementation is missing");

  const postImplementation = source.slice(postStart, postEnd);
  const dragImplementation = source.slice(dragStart, dragEnd);
  assert.match(postImplementation, /event\.postToPid\(processID\)/);
  assert.match(postImplementation, /event\.timestamp = CGEventTimestamp\(mach_absolute_time\(\)\)/);
  assert.doesNotMatch(postImplementation, /\.post\(tap:/);
  assert.doesNotMatch(`${postImplementation}\n${dragImplementation}`, /\bactivate(?:Application)?\s*\(/);
  assert.match(dragImplementation, /windowTargetedMouseEvent/);
  assert.match(dragImplementation, /fromLocal:/);
  assert.match(dragImplementation, /toLocal:/);
  assert.match(
    dragImplementation,
    /guard let focusStealSuppression = FocusStealSuppression\(targetProcessID: processID\)/,
  );
  assert.match(dragImplementation, /let clickEventNumber = SyntheticMouseEventNumbers\.next\(\)/);
  assert.match(dragImplementation, /let dragEventNumber = SyntheticMouseEventNumbers\.next\(\)/);
  assert.match(dragImplementation, /type: \.leftMouseDown[\s\S]*eventNumber: clickEventNumber/);
  assert.match(dragImplementation, /type: \.leftMouseDragged[\s\S]*eventNumber: dragEventNumber/);
  assert.match(dragImplementation, /type: \.leftMouseUp[\s\S]*eventNumber: clickEventNumber/);
  assert.doesNotMatch(dragImplementation, /TargetProcessActivityNotifications/);
  assert.doesNotMatch(dragImplementation, /appActivatedSubtype|appDeactivatedSubtype/);

  const pointerStreamStart = dragImplementation.indexOf("let clickEventNumber");
  const pointerStreamEnd = dragImplementation.lastIndexOf("post(up, to: processID)");
  assert.ok(
    pointerStreamStart >= 0 && pointerStreamEnd > pointerStreamStart,
    "window-targeted pointer stream is incomplete",
  );
  assert.doesNotMatch(
    dragImplementation.slice(pointerStreamStart, pointerStreamEnd),
    /usleep\(/,
  );
  assert.match(
    dragImplementation.slice(0, pointerStreamStart),
    /defer \{[\s\S]*usleep\(100_000\)[\s\S]*focusStealSuppression\.stop\(\)/,
  );

  const bridgeStart = source.indexOf("private enum WindowServerEventBridge");
  const bridgeEnd = source.indexOf("private enum TargetProcessInputRouting", bridgeStart);
  assert.ok(bridgeStart >= 0 && bridgeEnd > bridgeStart, "window-local event bridge is missing");
  const bridgeImplementation = source.slice(bridgeStart, bridgeEnd);
  assert.match(bridgeImplementation, /CGEventSetWindowLocation/);
  assert.match(bridgeImplementation, /setLocalLocation/);
  assert.doesNotMatch(bridgeImplementation, /SLEventPostToPid/);
  assert.doesNotMatch(bridgeImplementation, /SLEventSetIntegerValueField/);
  assert.doesNotMatch(bridgeImplementation, /\.post\(tap:/);
  assert.doesNotMatch(bridgeImplementation, /\bactivate(?:Application)?\s*\(/);

  const routingStart = source.indexOf("private enum TargetProcessInputRouting");
  const routingEnd = source.indexOf("private final class FocusStealSuppressionState", routingStart);
  assert.ok(routingStart >= 0 && routingEnd > routingStart, "key-focus routing event is missing");
  const routingImplementation = source.slice(routingStart, routingEnd);
  assert.match(routingImplementation, /processNotificationType: UInt = 21/);
  assert.match(routingImplementation, /appKitDefinedType: UInt = 13/);
  assert.match(routingImplementation, /keyFocusReturnedSubtype: UInt16 = 0x8000/);
  assert.match(routingImplementation, /inactiveWindowActivatedSubtype: UInt16 = 1/);
  assert.match(routingImplementation, /appDeactivatedSubtype: UInt16 = 2/);
  assert.match(
    routingImplementation,
    /inactiveWindowModifiers = NSEvent\.ModifierFlags\(rawValue: 0xC0000\)/,
  );
  assert.match(routingImplementation, /NSEvent\.otherEvent\(/);
  assert.match(routingImplementation, /windowNumber: 0/);
  assert.match(routingImplementation, /event\.postToPid\(processID\)/);
  const inactiveRouteStart = routingImplementation.indexOf(
    "subtype: inactiveWindowActivatedSubtype",
  );
  const inactiveRouteEnd = routingImplementation.indexOf(
    "static func deactivateCurrentRoute",
    inactiveRouteStart,
  );
  const inactiveRouteCall = routingImplementation.slice(inactiveRouteStart, inactiveRouteEnd);
  assert.match(
    inactiveRouteCall,
    /windowNumber: 0,[\s\S]*modifierFlags: \[\],[\s\S]*subtype: keyFocusReturnedSubtype/,
  );
  const activatedSubtypeMarker = "subtype: inactiveWindowActivatedSubtype";
  const firstActivatedMarker = inactiveRouteCall.indexOf(activatedSubtypeMarker);
  const flaggedWindowRouteStart = inactiveRouteCall.indexOf(
    activatedSubtypeMarker,
    firstActivatedMarker + activatedSubtypeMarker.length,
  );
  const flaggedWindowRouteCall = inactiveRouteCall.slice(flaggedWindowRouteStart);
  assert.match(flaggedWindowRouteCall, /windowNumber: Int\(windowID\)/);
  assert.match(flaggedWindowRouteCall, /modifierFlags: inactiveWindowModifiers/);
  assert.doesNotMatch(flaggedWindowRouteCall, /modifierFlags: \[\]/);
  assert.doesNotMatch(routingImplementation, /\.activate\s*\(/);
  assert.match(
    dragImplementation,
    /TargetProcessInputRouting\.ensureInactiveWindowRouting\([\s\S]*windowID: windowID,[\s\S]*guardedBy: focusStealSuppression/,
  );
  assert.doesNotMatch(dragImplementation, /deactivateCurrentRoute|endInactiveWindowRouting/);
  assert.match(
    source,
    /static func serveMCP[\s\S]*defer \{ TargetProcessInputRouting\.deactivateCurrentRoute\(\) \}/,
  );
  assert.match(
    routingImplementation,
    /currentRoute\.processID == processID[\s\S]*currentRoute\.windowID == windowID[\s\S]*currentRoute\.observedFrontmostProcessID == frontmostProcessID/,
  );

  const targetedMouseStart = source.indexOf("private func windowTargetedMouseEvent(");
  const targetedMouseEnd = source.indexOf("private func mouseEvent(", targetedMouseStart);
  assert.ok(
    targetedMouseStart >= 0 && targetedMouseEnd > targetedMouseStart,
    "window-targeted NSEvent construction is missing",
  );
  const targetedMouseImplementation = source.slice(targetedMouseStart, targetedMouseEnd);
  assert.match(targetedMouseImplementation, /NSEvent\.mouseEvent\(/);
  assert.match(targetedMouseImplementation, /windowNumber: Int\(windowID\)/);
  assert.match(targetedMouseImplementation, /eventNumber: Int\(eventNumber\)/);
  assert.match(targetedMouseImplementation, /clickCount: clickCount/);
  assert.match(targetedMouseImplementation, /pressure: 1/);
  assert.match(
    targetedMouseImplementation,
    /CGEventField\(rawValue: 3\)![\s\S]*Int64\(button\.rawValue\)/,
  );
  assert.match(
    targetedMouseImplementation,
    /CGEventField\(rawValue: 7\)![\s\S]*value: 3/,
  );
  assert.match(targetedMouseImplementation, /mouseEventWindowUnderMousePointer/);
  assert.match(targetedMouseImplementation, /mouseEventWindowUnderMousePointerThatCanHandleThisEvent/);
  assert.match(targetedMouseImplementation, /WindowServerEventBridge\.setLocalLocation/);
  assert.doesNotMatch(targetedMouseImplementation, /CGEvent\(\s*mouseEventSource:/);
  assert.doesNotMatch(targetedMouseImplementation, /\.post\(tap:/);

  const suppressionStart = source.indexOf("private final class FocusStealSuppressionState");
  const suppressionEnd = source.indexOf("private enum SyntheticMouseEventNumbers", suppressionStart);
  assert.ok(
    suppressionStart >= 0 && suppressionEnd > suppressionStart,
    "focus-steal prevention tap is missing",
  );
  const suppressionImplementation = source.slice(suppressionStart, suppressionEnd);
  assert.match(suppressionImplementation, /cgAnnotatedSessionEventTap/);
  assert.match(suppressionImplementation, /CGEventMask\(1\).*CGEventType\(rawValue: 21\)/s);
  assert.match(suppressionImplementation, /CGEventField\(rawValue: 40\)/);
  assert.match(suppressionImplementation, /CGEventField\(rawValue: 41\)/);
  assert.match(suppressionImplementation, /CGEventField\(rawValue: 64\)/);
  assert.match(suppressionImplementation, /CGEventField\(rawValue: 73\)/);
  assert.match(suppressionImplementation, /sourceProcessID == getpid\(\) && subtype == 0x8000/);
  assert.match(suppressionImplementation, /sourceProcessID == state\.targetProcessID/);
  assert.match(suppressionImplementation, /targetProcessID == state\.targetProcessID/);
  assert.match(suppressionImplementation, /subjectProcessID == state\.targetProcessID/);
  assert.doesNotMatch(suppressionImplementation, /\bactivate(?:Application)?\s*\(/);

  assert.doesNotMatch(source, /private enum TargetProcessActivityNotifications/);
  assert.equal((source.match(/NSEvent\.otherEvent\(/g) ?? []).length, 1);
  assert.doesNotMatch(source, /SLEventPostToPid|SLEventSetIntegerValueField/);
  assert.doesNotMatch(source, /FrontmostApplicationSnapshot/);
  assert.match(source, /configuration\.activates = false/);
});
