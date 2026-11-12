"use client";
import { Suspense, useEffect, useMemo, useRef } from "react";
import { Canvas, useFrame } from "@react-three/fiber";
import { Edges, Environment, Html, OrbitControls, useTexture } from "@react-three/drei";
import * as THREE from "three";

const INK = "#171717";
const PAPER = "#ece8e1";
const NIGHT = "#0c0a10";
const STONE2 = "#6f6259";
const SAND = "#c49a4c";
const RED = "#f2735f";
const RED2 = "#a94a3c";
const BLUE = "#829efe";
const BLUE2 = "#5368c4";
const BLOOD = ["#9e0016", "#c1001c", "#7a0010"];
const SKIN = ["#f3c9a5", "#d9a37c", "#a86f4c", "#7a4a2e", "#ffe0c2"];

export type Fighter = { ticker: string; icon: string | null };
export type Hit = { id: number; side: "a" | "b"; label: string };
export type Bubble = { side: "a" | "b"; text: string } | null;
/** A real war result from the feed (or its replay): who got knocked out, or a draw. */
export type Outcome = { id: string; kind: "ko"; loser: "a" | "b" } | { id: string; kind: "void" } | null;

type Shared = {
  hitAt: { a: number; b: number };   // when each side last landed a punch
  count: { a: number; b: number };   // punches so far (alternates lead / rear hand)
  koAt: number; loser: "a" | "b" | null; voidAt: number;
  shake: number;
};

const now = () => performance.now();
/** Deterministic pseudo-random in [0, 1): same crowd and same blood every render. */
const rand = (seed: number) => { const x = Math.sin(seed * 12.9898 + 78.233) * 43758.5453; return x - Math.floor(x); };
const ease = (t: number) => 1 - Math.pow(1 - Math.min(1, Math.max(0, t)), 3);
const up = new THREE.Vector3(0, 1, 0);

/** Puts a unit capsule between two points (arms, legs) each frame. */
function placeLimb(m: THREE.Object3D, a: THREE.Vector3, b: THREE.Vector3) {
  const d = new THREE.Vector3().subVectors(b, a);
  m.position.copy(a).addScaledVector(d, 0.5);
  m.quaternion.setFromUnitVectors(up, d.clone().normalize());
  m.scale.set(1, d.length(), 1);
}

// ---------------------------------------------------------------- stadium

/** Real (CC0, Poly Haven) travertine and sand, tiled. `repeat` sets how many times the 1K texture repeats. */
function useSurface(name: "stone" | "sand", repeat: [number, number]) {
  const t = useTexture({ map: `/arena/${name}_diff.jpg`, normalMap: `/arena/${name}_nor.jpg`, roughnessMap: `/arena/${name}_rough.jpg` });
  return useMemo(() => {
    const out: Record<string, THREE.Texture> = {};
    for (const [k, tex] of Object.entries(t)) {
      const c = tex.clone();
      c.wrapS = c.wrapT = THREE.RepeatWrapping;
      c.repeat.set(repeat[0], repeat[1]);
      c.anisotropy = 4;
      if (k === "map") c.colorSpace = THREE.SRGBColorSpace;
      c.needsUpdate = true;
      out[k] = c;
    }
    return out as { map: THREE.Texture; normalMap: THREE.Texture; roughnessMap: THREE.Texture };
  }, [t, repeat]);
}

/** Stepped stone bowl: the seating tiers, then the arcade on top. */
function Stadium() {
  const bowl = useSurface("stone", useMemo(() => [24, 3], []));
  const pier = useSurface("stone", useMemo(() => [0.6, 1.4], []));
  const sand = useSurface("sand", useMemo(() => [5, 5], []));
  const lathe = useMemo(() => {
    const pts: THREE.Vector2[] = [new THREE.Vector2(8.4, 0), new THREE.Vector2(8.4, 0.9)];
    for (let i = 0; i < 8; i++) {
      const r = 8.4 + i * 1.05, y = 0.9 + i * 0.72;
      pts.push(new THREE.Vector2(r + 1.05, y), new THREE.Vector2(r + 1.05, y + 0.72));
    }
    pts.push(new THREE.Vector2(17.5, 6.66), new THREE.Vector2(17.5, 0));
    return new THREE.LatheGeometry(pts, 96);
  }, []);
  const arches = useMemo(() => {
    const out: { pos: [number, number, number]; rot: number }[] = [];
    const n = 44;
    for (let i = 0; i < n; i++) {
      const a = (i / n) * Math.PI * 2;
      out.push({ pos: [Math.cos(a) * 17.1, 7.9, Math.sin(a) * 17.1], rot: -a + Math.PI / 2 });
    }
    return out;
  }, []);
  return (
    <group>
      <mesh geometry={lathe} receiveShadow>
        <meshStandardMaterial {...bowl} color="#d9cbb4" side={THREE.DoubleSide} />
      </mesh>
      {/* the arena wall the fighters stand inside */}
      <mesh position={[0, 0.45, 0]}>
        <cylinderGeometry args={[8.33, 8.33, 0.9, 96, 1, true]} />
        <meshStandardMaterial {...pier} color="#b98a6e" side={THREE.DoubleSide} />
      </mesh>
      {arches.map((p, i) => (
        <group key={i} position={p.pos} rotation={[0, p.rot, 0]}>
          {[-1, 1].map((x) => (
            <mesh key={x} position={[x, 0, 0]} castShadow>
              <boxGeometry args={[0.5, 2.6, 0.7]} />
              <meshStandardMaterial {...pier} color="#cdbca3" />
            </mesh>
          ))}
          <mesh position={[0, 1.45, 0]} castShadow>
            <boxGeometry args={[2.55, 0.4, 0.7]} />
            <meshStandardMaterial {...pier} color="#d9cbb4" />
          </mesh>
        </group>
      ))}
      <mesh rotation={[-Math.PI / 2, 0, 0]} position={[0, 0, 0]} receiveShadow>
        <circleGeometry args={[8.4, 96]} />
        <meshStandardMaterial {...sand} color="#e8cf9e" />
      </mesh>
      <mesh rotation={[-Math.PI / 2, 0, 0]} position={[0, 0.006, 0]}>
        <ringGeometry args={[4.6, 4.8, 96]} />
        <meshBasicMaterial color={INK} />
      </mesh>
    </group>
  );
}

/** ~600 fans on the tiers. Red-corner fans sit on the left, blue on the right; each half jumps when its fighter lands a hit. */
function Crowd({ shared }: { shared: React.RefObject<Shared> }) {
  const body = useRef<THREE.InstancedMesh>(null);
  const head = useRef<THREE.InstancedMesh>(null);
  const seats = useMemo(() => {
    const out: { x: number; y: number; z: number; side: "a" | "b" | "n"; ph: number; sp: number }[] = [];
    for (let row = 0; row < 8; row++) {
      const r = 8.4 + row * 1.05 + 0.6, y = 0.9 + row * 0.72;
      const n = Math.floor(2 * Math.PI * r / 0.78);
      for (let i = 0; i < n; i++) {
        const a = (i / n) * Math.PI * 2 + row * 0.13;
        const x = Math.cos(a) * r, z = Math.sin(a) * r;
        if (z > 6.5 && Math.abs(x) < 4) continue; // keep the near side open for the camera
        const k = row * 1000 + i;
        if (rand(k) < 0.12) continue;             // empty seats
        const side = Math.abs(x) < 1.2 ? "n" : x < 0 ? "a" : "b";
        out.push({ x, y, z, side, ph: rand(k + 0.5) * 6.28, sp: 0.6 + rand(k + 0.7) * 0.8 });
      }
    }
    return out;
  }, []);

  useEffect(() => {
    const c = new THREE.Color();
    seats.forEach((s, i) => {
      const shirts = s.side === "a" ? [RED, RED, RED2, PAPER] : s.side === "b" ? [BLUE, BLUE, BLUE2, PAPER] : [PAPER, SAND, STONE2];
      body.current!.setColorAt(i, c.set(shirts[i % shirts.length]));
      head.current!.setColorAt(i, c.set(SKIN[(i * 7) % SKIN.length]));
    });
    body.current!.instanceColor!.needsUpdate = true;
    head.current!.instanceColor!.needsUpdate = true;
  }, [seats]);

  const o = useMemo(() => new THREE.Object3D(), []);
  useFrame(({ clock }) => {
    const t = clock.getElapsedTime();
    const sh = shared.current;
    const hype = (side: "a" | "b") => {
      const since = now() - sh.hitAt[side];
      const ko = sh.loser && sh.loser !== side ? Math.max(0, 1 - (now() - sh.koAt) / 5000) : 0;
      return Math.max(Math.max(0, 1 - since / 1400), ko);
    };
    const ha = hype("a"), hb = hype("b");
    seats.forEach((s, i) => {
      const h = s.side === "a" ? ha : s.side === "b" ? hb : Math.max(ha, hb) * 0.5;
      const jump = h * Math.abs(Math.sin(t * 9 * s.sp + s.ph)) * 0.55;
      const idle = Math.sin(t * 1.4 * s.sp + s.ph) * 0.03;
      const lookAt = Math.atan2(-s.x, -s.z);
      o.position.set(s.x, s.y + 0.42 + jump + idle, s.z);
      o.rotation.set(0, lookAt, 0);
      o.updateMatrix();
      body.current!.setMatrixAt(i, o.matrix);
      o.position.y += 0.47;
      o.updateMatrix();
      head.current!.setMatrixAt(i, o.matrix);
    });
    body.current!.instanceMatrix.needsUpdate = true;
    head.current!.instanceMatrix.needsUpdate = true;
  });

  return (
    <group>
      <instancedMesh ref={body} args={[undefined, undefined, seats.length]}>
        <capsuleGeometry args={[0.2, 0.32, 3, 8]} />
        <meshToonMaterial />
      </instancedMesh>
      <instancedMesh ref={head} args={[undefined, undefined, seats.length]}>
        <sphereGeometry args={[0.17, 10, 8]} />
        <meshToonMaterial />
      </instancedMesh>
    </group>
  );
}

// ---------------------------------------------------------------- fighters

function useFace(src: string | null, ticker: string) {
  const tex = useMemo(() => {
    let t: THREE.Texture;
    if (src) t = new THREE.TextureLoader().load(src);
    else {
      const c = document.createElement("canvas");
      c.width = c.height = 256;
      const g = c.getContext("2d")!;
      g.fillStyle = PAPER; g.fillRect(0, 0, 256, 256);
      g.fillStyle = INK; g.font = "150px 'Bagel Fat One', sans-serif"; g.textAlign = "center"; g.textBaseline = "middle";
      g.fillText(ticker.slice(0, 1), 128, 140);
      t = new THREE.CanvasTexture(c);
    }
    t.colorSpace = THREE.SRGBColorSpace;
    return t;
  }, [src, ticker]);
  useEffect(() => () => tex.dispose(), [tex]);
  return tex;
}

/** A coin with arms, gloves and legs. It bounces like a boxer, jabs on every buy, gets knocked out flat when it loses. */
function Brawler({ side, fighter, shared }: { side: "a" | "b"; fighter: Fighter; shared: React.RefObject<Shared> }) {
  const root = useRef<THREE.Group>(null);
  const torso = useRef<THREE.Group>(null);
  const limbs = useRef<(THREE.Mesh | null)[]>([]);
  const gloves = useRef<(THREE.Mesh | null)[]>([]);
  const boots = useRef<(THREE.Mesh | null)[]>([]);
  const blood = useRef<THREE.Group>(null);
  const face = useFace(fighter.icon, fighter.ticker);
  const dir = side === "a" ? 1 : -1;
  const other = side === "a" ? "b" : "a";
  const corner = side === "a" ? RED : BLUE;
  const homeX = -dir * 2.05;
  // A big pool under the body plus splatter thrown toward the camera, so it reads from the stands.
  const pools = useMemo(() => Array.from({ length: 12 }, (_, i) => {
    const main = i < 5;
    return {
      x: (rand(i + dir * 10) - 0.5) * (main ? 1.8 : 4.2),
      z: main ? -1.4 - rand(i + 50) * 1.6 : -0.6 + rand(i + 50) * 2.6,
      r: main ? 0.9 + rand(i + 90) * 0.7 : 0.12 + rand(i + 90) * 0.3,
      c: BLOOD[i % 3], d: main ? i * 0.15 : 0.1 + (i - 5) * 0.05,
    };
  }), [dir]);

  useFrame(({ clock }) => {
    const sh = shared.current;
    const t = clock.getElapsedTime();
    const n = now();
    const g = root.current, tor = torso.current;
    if (!g || !tor) return;
    const punch = Math.max(0, 1 - (n - sh.hitAt[side]) / 320);          // 1 → 0 over the jab
    const reach = Math.sin(Math.min(1, punch) * Math.PI);
    const ouch = Math.max(0, 1 - (n - sh.hitAt[other]) / 500);
    const lead = sh.count[side] % 2 === 0;
    const koT = sh.loser === side ? ease((n - sh.koAt) / 900) : 0;
    const winT = sh.loser === other ? Math.max(0, 1 - (n - sh.koAt) / 6000) : 0;
    const shrug = Math.max(0, 1 - (n - sh.voidAt) / 3500);
    const bounce = koT > 0 ? 0 : Math.abs(Math.sin(t * 5.5 + (side === "a" ? 0 : 1.3))) * 0.12 + winT * Math.abs(Math.sin(t * 8)) * 0.6;

    g.position.set(homeX + dir * reach * 0.35 - dir * ouch * 0.4, koT * 0.17, 0);
    // knocked out: falls on its back, face up, away from the camera
    g.rotation.set(-koT * Math.PI / 2, 0, -dir * ouch * 0.18 * (1 - koT));
    tor.position.y = 2.15 + bounce;

    const S = (x: number, y: number, z = 0) => new THREE.Vector3(x, y, z);
    const shL = S(dir * 0.92, 2.2 + bounce, 0.1), shR = S(-dir * 0.92, 2.05 + bounce, 0.1);
    let gL = S(dir * 1.3, 2.45 + bounce, 0.45), gR = S(-dir * 1.2, 1.85 + bounce, 0.55);
    if (lead) gL = gL.lerp(S(dir * 2.75, 2.3 + bounce, 0.25), reach);
    else gR = gR.lerp(S(dir * 2.45, 2.1 + bounce, 0.6), reach);
    if (winT > 0) { gL = S(dir * 0.8, 3.7 + bounce, 0.2); gR = S(-dir * 0.8, 3.7 + bounce, 0.2); }
    if (shrug > 0 && !sh.loser) { gL = gL.lerp(S(dir * 1.5, 2.9, 0.4), shrug); gR = gR.lerp(S(-dir * 1.5, 2.9, 0.4), shrug); }
    const step = Math.sin(t * 5.5 + (side === "a" ? 0 : 1.3)) * 0.12 * (1 - koT);
    const hipL = S(0.32, 1.15 + bounce * 0.6), hipR = S(-0.32, 1.15 + bounce * 0.6);
    const ftL = S(0.5 + step, 0.12, 0.05), ftR = S(-0.5 + step, 0.12, -0.05);

    const L = limbs.current;
    if (L[0]) placeLimb(L[0], shL, gL);
    if (L[1]) placeLimb(L[1], shR, gR);
    if (L[2]) placeLimb(L[2], hipL, ftL);
    if (L[3]) placeLimb(L[3], hipR, ftR);
    gloves.current[0]?.position.copy(gL);
    gloves.current[1]?.position.copy(gR);
    boots.current[0]?.position.copy(ftL);
    boots.current[1]?.position.copy(ftR);

    // blood pool spreads under the loser
    if (blood.current) {
      blood.current.visible = koT > 0;
      blood.current.children.forEach((c, i) => {
        const k = ease((n - sh.koAt - 700 - pools[i].d * 1000) / 1600);
        c.scale.setScalar(Math.max(0.001, k));
      });
    }
  });

  return (
    <group>
      <group ref={root}>
        <group ref={torso}>
          <mesh rotation={[Math.PI / 2, 0, 0]}>
            <cylinderGeometry args={[1, 1, 0.3, 48]} />
            <meshToonMaterial attach="material-0" color={corner} />
            <meshBasicMaterial attach="material-1" map={face} />
            <meshToonMaterial attach="material-2" color={corner} />
            <Edges color={INK} threshold={20} />
          </mesh>
          {/* belt */}
          <mesh position={[0, -1.02, 0]}>
            <boxGeometry args={[0.9, 0.16, 0.34]} />
            <meshToonMaterial color={INK} />
          </mesh>
        </group>
        {[0, 1, 2, 3].map((i) => (
          <mesh key={i} ref={(m) => { limbs.current[i] = m; }}>
            <cylinderGeometry args={[0.13, 0.13, 1, 10]} />
            <meshToonMaterial color={INK} />
          </mesh>
        ))}
        {[0, 1].map((i) => (
          <mesh key={i} ref={(m) => { gloves.current[i] = m; }}>
            <sphereGeometry args={[0.3, 18, 14]} />
            <meshToonMaterial color={corner} />
            <Edges color={INK} threshold={40} />
          </mesh>
        ))}
        {[0, 1].map((i) => (
          <mesh key={i} ref={(m) => { boots.current[i] = m; }} scale={[1.4, 0.7, 1]}>
            <sphereGeometry args={[0.2, 12, 10]} />
            <meshToonMaterial color={INK} />
          </mesh>
        ))}
        <Html center position={[0, 3.7, 0]} pointerEvents="none" zIndexRange={[10, 0]}>
          <div className="whitespace-nowrap rounded-full border border-ink bg-paper px-3 py-1 font-display text-lg shadow-[0_18px_40px_-22px_var(--color-drop)]">${fighter.ticker}</div>
        </Html>
      </group>
      <group ref={blood} position={[homeX, 0.012, 0]} visible={false}>
        {pools.map((p, i) => (
          <mesh key={i} position={[p.x, i * 0.001, p.z]} rotation={[-Math.PI / 2, 0, 0]}>
            <circleGeometry args={[p.r, 24]} />
            <meshBasicMaterial color={p.c} />
          </mesh>
        ))}
      </group>
    </group>
  );
}

/** Small camera shake on every punch. */
function Shake({ shared }: { shared: React.RefObject<Shared> }) {
  // OrbitControls rewrites the camera position every frame, so this jitter never accumulates.
  useFrame((state) => {
    const k = Math.max(0, 1 - (now() - shared.current.shake) / 250);
    if (k > 0) state.camera.position.y += (Math.random() - 0.5) * 0.08 * k;
  });
  return null;
}

const POW = ["POW!", "BAM!", "WHAM!", "BONK!", "SMACK!"];

export default function Arena3D({ a, b, hits, outcome, practice }: { a: Fighter; b: Fighter; hits: Hit[]; outcome: Outcome; practice: boolean }) {
  const shared = useRef<Shared>({ hitAt: { a: -1e9, b: -1e9 }, count: { a: 0, b: 0 }, koAt: -1e9, loser: null, voidAt: -1e9, shake: -1e9 });
  const seen = useRef(new Set<number>());
  const done = useRef<string | null>(null);

  useEffect(() => {
    for (const h of hits) {
      if (seen.current.has(h.id)) continue;
      seen.current.add(h.id);
      shared.current.hitAt[h.side] = now();
      shared.current.count[h.side] += 1;
      shared.current.shake = now();
    }
  }, [hits]);

  useEffect(() => {
    if (!outcome || done.current === outcome.id) return;
    done.current = outcome.id;
    if (outcome.kind === "ko") Object.assign(shared.current, { loser: outcome.loser, koAt: now(), voidAt: -1e9 });
    else Object.assign(shared.current, { loser: null, voidAt: now() });
  }, [outcome]);

  // Phones: no shadow maps (the sky and the textures carry the look).
  const mobile = typeof window !== "undefined" && window.matchMedia("(max-width: 768px)").matches;

  // A new pair of fighters starts fresh.
  useEffect(() => {
    Object.assign(shared.current, { loser: null, koAt: -1e9, voidAt: -1e9 });
  }, [a.ticker, b.ticker]);

  return (
    <Canvas dpr={[1, 1.75]} shadows={mobile ? false : { type: THREE.PCFShadowMap }} camera={{ position: [0, 5.4, 12.2], fov: 40 }} gl={{ toneMapping: THREE.ACESFilmicToneMapping, toneMappingExposure: 1.05 }}>
      <color attach="background" args={[NIGHT]} />
      <fog attach="fog" args={["#e2b98c", 40, 80]} />
      {/* golden hour: a real sky panorama lights the stone (CC0, Poly Haven), the low sun throws long shadows */}
      <Suspense fallback={null}>
        <Environment files="/arena/sky_1k.hdr" background backgroundBlurriness={0.04} environmentIntensity={0.9} />
      </Suspense>
      <directionalLight position={[-14, 11, 6]} intensity={2.4} color="#ffd2a1" castShadow={!mobile} shadow-mapSize={[1024, 1024]} shadow-camera-left={-12} shadow-camera-right={12} shadow-camera-top={12} shadow-camera-bottom={-12} />
      <hemisphereLight args={["#ffe9cf", "#5a4636", 0.35]} />
      <Suspense fallback={null}>
        <Stadium />
      </Suspense>
      <Crowd shared={shared} />
      <Brawler side="a" fighter={a} shared={shared} />
      <Brawler side="b" fighter={b} shared={shared} />
      {hits.slice(-3).map((h, i) => (
        <Html key={h.id} center position={[h.side === "a" ? 0.6 : -0.6, 3 + i * 0.2, 0.8]} pointerEvents="none">
          <div className="pop text-center">
            <p className="font-display text-4xl text-yolk [-webkit-text-stroke:2px_var(--color-drop)]">{POW[h.id % POW.length]}</p>
            <p className="mt-1 rounded-full border border-ink bg-mint px-2 py-0.5 font-display text-base">{h.label}{practice ? " · warm-up" : ""}</p>
          </div>
        </Html>
      ))}
      {outcome?.kind === "ko" && (
        <Html center position={[0, 3.4, 1.5]} pointerEvents="none">
          <p key={outcome.id} className="pop-slow font-display text-8xl text-red [-webkit-text-stroke:3px_var(--color-drop)]">K.O.</p>
        </Html>
      )}
      {outcome?.kind === "void" && (
        <Html center position={[0, 3.4, 1.5]} pointerEvents="none">
          <p key={outcome.id} className="pop-slow whitespace-nowrap font-display text-5xl text-cream [-webkit-text-stroke:3px_var(--color-drop)]">NO CONTEST</p>
        </Html>
      )}
      <Shake shared={shared} />
      <OrbitControls enablePan={false} enableZoom={false} minPolarAngle={Math.PI / 3.4} maxPolarAngle={Math.PI / 2.3} minAzimuthAngle={-0.6} maxAzimuthAngle={0.6} autoRotate autoRotateSpeed={0.2} target={[0, 2, 0]} />
    </Canvas>
  );
}
