// Runs the KWin placement script (`link-router kwin-script`) against a mock of
// KWin's scripting API and checks where the player window ends up.
//   link-router kwin-script | node tests/kwin-placement.mjs
import { readFileSync } from "node:fs";
import assert from "node:assert/strict";

const source = readFileSync(0, "utf8");

function signal() {
    const handlers = [];
    return { connect: (f) => handlers.push(f), emit: (...a) => handlers.forEach((f) => f(...a)) };
}

let nextId = 1;
function makeWindow(resourceClass, { x = 100, y = 100, width = 400, height = 300, output, desktop, plainObjects = true } = {}) {
    const w = {
        internalId: `{${nextId++}}`,
        resourceClass,
        resourceName: "mpv",
        output,
        desktops: [desktop],
        keepAbove: false,
        move: false,
        resize: false,
        x, y, width, height,
        frameGeometryChanged: signal(),
        captionChanged: signal(),
        closed: signal(),
    };
    Object.defineProperty(w, "frameGeometry", {
        get: () => ({ x: w.x, y: w.y, width: w.width, height: w.height, __native: true }),
        set: (g) => {
            // plainObjects=false mimics a KWin whose geometry type only accepts its own values.
            if (!plainObjects && !g.__native) return;
            const changed = g.x !== w.x || g.y !== w.y || g.width !== w.width || g.height !== w.height;
            Object.assign(w, { x: g.x, y: g.y, width: g.width, height: g.height });
            if (changed) w.frameGeometryChanged.emit();
        },
    });
    return w;
}

function run(src, { followFocus = true } = {}) {
    const screen1 = { name: "DP-1", area: { x: 0, y: 0, width: 1920, height: 1040 } };
    const screen2 = { name: "HDMI-1", area: { x: 1920, y: 0, width: 2560, height: 1440 } };
    const desk1 = { id: "d1" }, desk2 = { id: "d2" };
    const windows = [];
    const workspace = {
        activeScreen: screen1,
        currentDesktop: desk1,
        windowAdded: signal(),
        windowList: () => windows.slice(),
        clientArea: (_option, output) => output.area,
    };
    const env = { workspace, windows, screen1, screen2, desk1, desk2 };
    const script = followFocus ? src : src.replace("const FOLLOW_FOCUS = true;", "const FOLLOW_FOCUS = false;");
    return { env, start: () => new Function("workspace", "KWin", "console", script)(workspace, { PlacementArea: 1 }, { info() {} }) };
}

const MX = Number(/const MARGIN_X = (-?\d+);/.exec(source)[1]);
const MY = Number(/const MARGIN_Y = (-?\d+);/.exec(source)[1]);
const APP = JSON.parse(/const APP_ID = ("(?:[^"\\]|\\.)*")/.exec(source)[1]);
const corner = (w, area) => [area.x + area.width - MX - w.width, area.y + area.height - MY - w.height];

// A player that exists before the script loads is picked up and anchored; others are left alone.
{
    const { env, start } = run(source);
    const player = makeWindow(APP, { output: env.screen1, desktop: env.desk1 });
    const other = makeWindow("firefox", { output: env.screen1, desktop: env.desk1 });
    env.windows.push(player, other);
    start();
    assert.deepEqual([player.x, player.y], corner(player, env.screen1.area));
    assert.equal(player.keepAbove, true);
    assert.deepEqual([other.x, other.y], [100, 100]);
}

// A new player window, mpv resizing it for the next video, a user drag, a new file on another desktop and screen.
{
    const { env, start } = run(source);
    start();
    const player = makeWindow(APP.toUpperCase(), { output: env.screen1, desktop: env.desk2 });
    env.workspace.windowAdded.emit(player);
    assert.deepEqual([player.x, player.y], corner(player, env.screen1.area), "anchored when it opens");
    assert.deepEqual(player.desktops, [env.desk1], "moved to the current desktop");

    player.frameGeometry = { x: player.x, y: player.y, width: 405, height: 720, __native: true };
    assert.deepEqual([player.x, player.y], corner(player, env.screen1.area), "re-anchored after mpv resized it");

    player.resize = true;
    player.frameGeometry = { x: 50, y: 60, width: 500, height: 400, __native: true };
    player.resize = false;
    assert.deepEqual([player.x, player.y], [50, 60], "a size the user drags stays");

    env.workspace.currentDesktop = env.desk2;
    env.workspace.activeScreen = env.screen2;
    player.captionChanged.emit();
    assert.deepEqual(player.desktops, [env.desk2]);
    assert.deepEqual([player.x, player.y], corner(player, env.screen2.area), "follows to the active screen on a new file");

    player.closed.emit();
    const again = makeWindow(APP, { output: env.screen2, desktop: env.desk2 });
    again.internalId = player.internalId;
    env.workspace.windowAdded.emit(again);
    assert.deepEqual([again.x, again.y], corner(again, env.screen2.area), "a reopened window is hooked again");
}

// Without follow_focus a new file doesn't move the window to another desktop or screen.
{
    const { env, start } = run(source, { followFocus: false });
    start();
    const player = makeWindow(APP, { output: env.screen1, desktop: env.desk1 });
    env.workspace.windowAdded.emit(player);
    env.workspace.currentDesktop = env.desk2;
    env.workspace.activeScreen = env.screen2;
    player.captionChanged.emit();
    assert.deepEqual(player.desktops, [env.desk1]);
    assert.deepEqual([player.x, player.y], corner(player, env.screen1.area));
}

// Geometry types that ignore plain objects get a modified copy of the window's own value.
{
    const { env, start } = run(source);
    start();
    const player = makeWindow(APP, { output: env.screen1, desktop: env.desk1, plainObjects: false });
    env.workspace.windowAdded.emit(player);
    assert.deepEqual([player.x, player.y], corner(player, env.screen1.area));
}

console.log("kwin placement script: all checks passed");
