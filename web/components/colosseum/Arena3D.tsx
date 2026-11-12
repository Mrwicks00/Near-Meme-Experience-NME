"use client";
import { Suspense, useEffect, useMemo, useRef } from "react";
import { Canvas, useFrame } from "@react-three/fiber";
import { Edges, Environment, Html, OrbitControls, useGLTF, useTexture } from "@react-three/drei";
import { clone as cloneSkinned } from "three/examples/jsm/utils/SkeletonUtils.js";
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

/** The bowl is a real model ("Colosseum" by Vladyslav Holhanov, CC-BY-4.0, Sketchfab), compressed to ~240 KB.
 *  Model units: arena wall at r 172-176, seating tiers from r 178 (y -25) to r 246 (y 21), outer wall to y 47, floor y -47. */
const BOWL_SCALE = 0.05;
const BOWL_FLOOR = 47;
/** Height of the tier surface at model radius `rm`, in scene units. */
const tierY = (rm: number) => (-24 + (rm - 178) * 0.69 + BOWL_FLOOR) * BOWL_SCALE;

/** The model has no UVs: give every face a box projection (by its normal) so the stone texture wraps it, in model units.
 *  Its own flat arena floor (faces centred inside r 165, near the bottom) is dropped so our sand shows. */
function boxUv(src: THREE.BufferGeometry, tile: number, world: THREE.Matrix4): THREE.BufferGeometry {
  const flat = src.index ? src.toNonIndexed() : src.clone();
  const all = flat.getAttribute("position");
  const keep: number[] = [];
  const l = new THREE.Vector3(), w = new THREE.Vector3();
  for (let i = 0; i < all.count; i += 3) {
    let x = 0, y = 0, z = 0;
    for (let k = 0; k < 3; k++) { w.fromBufferAttribute(all, i + k).applyMatrix4(world); x += w.x / 3; y += w.y / 3; z += w.z / 3; }
    if (Math.hypot(x, z) < 165 && y < -40) continue;
    // fromBufferAttribute decodes the compressed (quantized) positions; the raw array would not
    for (let k = 0; k < 3; k++) { l.fromBufferAttribute(all, i + k); keep.push(l.x, l.y, l.z); }
  }
  const g = new THREE.BufferGeometry();
  g.setAttribute("position", new THREE.Float32BufferAttribute(keep, 3));
  const pos = g.getAttribute("position");
  const uv = new Float32Array(pos.count * 2);
  const a = new THREE.Vector3(), b = new THREE.Vector3(), c = new THREE.Vector3(), n = new THREE.Vector3(), e = new THREE.Vector3();
  for (let i = 0; i < pos.count; i += 3) {
    a.fromBufferAttribute(pos, i); b.fromBufferAttribute(pos, i + 1); c.fromBufferAttribute(pos, i + 2);
    n.subVectors(c, b).cross(e.subVectors(a, b));
    const ax = Math.abs(n.x), ay = Math.abs(n.y), az = Math.abs(n.z);
    for (let k = 0; k < 3; k++) {
      const p = k === 0 ? a : k === 1 ? b : c;
      const [u, v] = ay >= ax && ay >= az ? [p.x, p.z] : ax >= az ? [p.z, p.y] : [p.x, p.y];
      uv[(i + k) * 2] = u / tile;
      uv[(i + k) * 2 + 1] = v / tile;
    }
  }
  g.setAttribute("uv", new THREE.BufferAttribute(uv, 2));
  g.computeVertexNormals(); // non-indexed: flat normals per face, so the normal map reads as cut blocks
  return g;
}

function Bowl() {
  const { scene } = useGLTF("/arena/colosseum.glb");
  const stone = useSurface("stone", useMemo(() => [1, 1], []));
  const obj = useMemo(() => {
    const c = scene.clone(true);
    // Real travertine (CC0, Poly Haven), warm tint, catches the low sun.
    const mat = new THREE.MeshStandardMaterial({ ...stone, color: "#d9c3a0", roughness: 1, metalness: 0, side: THREE.DoubleSide });
    mat.normalScale.set(1.2, 1.2);
    c.updateMatrixWorld(true);
    c.traverse((o) => {
      const m = o as THREE.Mesh;
      if (!m.isMesh) return;
      m.geometry = boxUv(m.geometry, 24, m.matrixWorld); // one texture tile per ~1.2 scene units
      m.material = mat;
      m.castShadow = true;
      m.receiveShadow = true;
    });
    return c;
  }, [scene, stone]);
  return <primitive object={obj} scale={BOWL_SCALE} position={[0, BOWL_FLOOR * BOWL_SCALE, 0]} />;
}
useGLTF.preload("/arena/colosseum.glb");

/** The Colosseum around the sand: the real model's stands and walls. */
function Stadium() {
  const sand = useSurface("sand", useMemo(() => [5, 5], []));
  return (
    <group>
      <Bowl />
      <mesh rotation={[-Math.PI / 2, 0, 0]} position={[0, 0, 0]} receiveShadow>
        <circleGeometry args={[8.9, 96]} />
        <meshStandardMaterial {...sand} color="#e8cf9e" />
      </mesh>
      <mesh rotation={[-Math.PI / 2, 0, 0]} position={[0, 0.006, 0]}>
        <ringGeometry args={[4.6, 4.8, 96]} />
        <meshBasicMaterial color={INK} />
      </mesh>
    </group>
  );
}

/** ~500 fans on the tiers. Red-corner fans sit on the left, blue on the right; each half jumps when its fighter lands a hit. */
type Seat = { x: number; y: number; z: number; side: "a" | "b" | "n"; ph: number; sp: number; row: number };

/** Every seat of the bowl (same layout for the cheap crowd and the real fans). */
function useSeats(): Seat[] {
  return useMemo(() => {
    const out: Seat[] = [];
    for (let row = 0; row < 6; row++) {
      const rm = 181 + row * 12.4; // one row per ~0.62 scene units up the model's tiers
      const r = rm * BOWL_SCALE, y = tierY(rm);
      const n = Math.floor(2 * Math.PI * r / 0.78);
      for (let i = 0; i < n; i++) {
        const a = (i / n) * Math.PI * 2 + row * 0.13;
        const x = Math.cos(a) * r, z = Math.sin(a) * r;
        if (z > 6.5 && Math.abs(x) < 4) continue; // keep the near side open for the camera
        const k = row * 1000 + i;
        if (rand(k) < 0.12) continue;             // empty seats
        const side = Math.abs(x) < 1.2 ? "n" : x < 0 ? "a" : "b";
        out.push({ x, y, z, side, ph: rand(k + 0.5) * 6.28, sp: 0.6 + rand(k + 0.7) * 0.8, row });
      }
    }
    return out;
  }, []);
}

/** The seats the camera sees best (front rows, far side, every other one): real animated fans sit there. */
function realSeats(all: Seat[], max: number): Set<number> {
  const picks = all
    .map((s, i) => ({ s, i }))
    .filter(({ s, i }) => s.row <= 2 && s.z < -2.5 && Math.abs(s.x) < 9 && i % 2 === 0)
    .sort((p, q) => p.s.row - q.s.row || Math.abs(p.s.x) - Math.abs(q.s.x))
    .slice(0, max);
  return new Set(picks.map((p) => p.i));
}

/** Real people (Mixamo "Remy", "Sitting Clap") in the best seats; each claps harder when its coin lands a hit. */
type Fan = { obj: THREE.Object3D; mixer: THREE.AnimationMixer; action: THREE.AnimationAction; side: "a" | "b" | "n"; base: number };
function RealFans({ seats, shared }: { seats: Seat[]; shared: React.RefObject<Shared> }) {
  const gltf = useGLTF("/arena/fan.glb");
  const group = useRef<THREE.Group>(null);
  const fans = useRef<Fan[]>([]);

  // Built imperatively: three.js objects that animate every frame, owned by this component.
  useEffect(() => {
    const g = group.current;
    if (!g) return;
    // Size from the meshes' bind-pose bounds (never through the skeleton, so a bad rig can't break the page).
    gltf.scene.updateMatrixWorld(true);
    const box = new THREE.Box3();
    gltf.scene.traverse((o) => {
      const m = o as THREE.Mesh;
      if (!m.isMesh || !m.geometry) return;
      if (!m.geometry.boundingBox) m.geometry.computeBoundingBox();
      if (m.geometry.boundingBox) box.union(m.geometry.boundingBox.clone().applyMatrix4(m.matrixWorld));
    });
    const h = box.isEmpty() ? 0 : box.max.y - box.min.y;
    if (!gltf.animations[0] || h <= 0) return; // nothing usable: keep the cheap crowd only
    const scale = 1.05 / h;
    const tint = { a: new THREE.Color(RED), b: new THREE.Color(BLUE), n: new THREE.Color(PAPER) };
    const clip = gltf.animations[0];
    const list: Fan[] = seats.map((s, i) => {
      const obj = cloneSkinned(gltf.scene);
      obj.traverse((o) => {
        const m = o as THREE.Mesh;
        if (!m.isMesh) return;
        m.frustumCulled = false; // skinned meshes move outside their bind box
        if (/Tops/i.test(m.name) && m.material) {
          const mat = (m.material as THREE.MeshStandardMaterial).clone();
          mat.color = tint[s.side].clone();
          m.material = mat;
        }
      });
      obj.scale.setScalar(scale);
      obj.position.set(s.x, s.y - 0.12, s.z);
      obj.rotation.set(0, Math.atan2(-s.x, -s.z), 0);
      const mixer = new THREE.AnimationMixer(obj);
      const action = mixer.clipAction(clip);
      action.play();
      if (clip) action.time = rand(i + 9) * clip.duration;
      g.add(obj);
      return { obj, mixer, action, side: s.side, base: 0.85 + rand(i + 3) * 0.3 };
    });
    fans.current = list;
    return () => {
      for (const f of list) { f.mixer.stopAllAction(); g.remove(f.obj); }
      fans.current = [];
    };
  }, [gltf, seats]);

  useFrame((_, dt) => {
    const sh = shared.current;
    for (const f of fans.current) {
      const since = f.side === "n" ? Math.min(now() - sh.hitAt.a, now() - sh.hitAt.b) : now() - sh.hitAt[f.side];
      const hype = Math.max(0, 1 - since / 1600);
      f.action.setEffectiveTimeScale(f.base * (1 + hype * 1.6));
      f.mixer.update(Math.min(dt, 0.05));
    }
  });

  return <group ref={group} />;
}
useGLTF.preload("/arena/fan.glb");

function Crowd({ shared, all, skip }: { shared: React.RefObject<Shared>; all: Seat[]; skip: Set<number> }) {
  const body = useRef<THREE.InstancedMesh>(null);
  const head = useRef<THREE.InstancedMesh>(null);
  const seats = useMemo(() => all.filter((_, i) => !skip.has(i)), [all, skip]);

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

  // Phones: no shadow maps (the sky and the textures carry the look) and fewer real fans.
  const mobile = typeof window !== "undefined" && window.matchMedia("(max-width: 768px)").matches;
  const seats = useSeats();
  const fanIdx = useMemo(() => realSeats(seats, mobile ? 12 : 36), [seats, mobile]);
  const fanSeats = useMemo(() => seats.filter((_, i) => fanIdx.has(i)), [seats, fanIdx]);

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
      <Suspense fallback={null}>
        <RealFans seats={fanSeats} shared={shared} />
      </Suspense>
      <Crowd shared={shared} all={seats} skip={fanIdx} />
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
