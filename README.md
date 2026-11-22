# peugeot300mille v0.8

Boîte de glitch vidéo analogique pour Linux. La vidéo passe par un **vrai signal composite NTSC
simulé** (910 échantillons × 525 lignes à 4×fsc) et traverse un ampli vidéo avec feedback, un
mélangeur de synchro, une télévision qui doit retrouver l'image, et une caméra qui refilme l'écran.
Les rayures, les traînées de couleur et les décrochages ne sont pas dessinés : ils naissent dans le
circuit simulé et évoluent d'une image à l'autre.

Inspiré de l'analyse de Yurenaki (voir `../video-synthesis-mini-dv/ANALYSE-video-feedback.md`).
Le code est une implémentation à nous. Les valeurs des composants de l'ampli et du mélangeur de
synchro ne sont pas celles du plugin : elles sont réglables dans `src/engine/amp.rs` (`TUNING`) et
`src/engine/syncmix.rs`.

## Prérequis

- Rust (stable), `ffmpeg` et `ffprobe` dans le PATH.

## Lancer

```sh
cargo build --release
./target/release/peugeot300mille                 # ouvre la fenêtre
./target/release/peugeot300mille ma_video.mp4    # ouvre directement une vidéo
```

### Dans la fenêtre

Le panneau de gauche a trois onglets : **Image** (entrée, ampli, VHS, sortie), **Caméra** et
**Son**. À côté de chaque bouton, le petit bouton **🎵** montre et règle son lien avec le son.
Double-clic sur un curseur : retour à sa valeur de départ. L'interface utilise IBM Plex si la
police est installée.

- **📂 Ouvrir…**, **▶ Lecture / ⏸ Pause** (barre Espace), barre de temps pour se déplacer.
- Boutons à gauche : groupes **Amp** (Enhancer Gain/Tone, Feedback Gain/Tone, Sync) et
  **Video Feedback** (Mix, Blur, Color, Noise, Camera). Tous à 0 = passage propre dans le circuit.
- **⏺ Enregistrer en direct…** : écrit ce qui est joué dans un MP4, avec le son d'origine.
  La lecture ne saute jamais d'image : si la machine est lente, l'aperçu ralentit, et le fichier
  reste exact.
- **⏺ Enregistrer les réglages** : joue la vidéo et retient chaque mouvement de bouton, image par
  image. Ensuite **🎬 Rendre l'enregistrement…** refait le rendu hors ligne, à pleine qualité,
  identique à ce qui a été joué. Les réglages enregistrés sont sauvés à côté
  (`sortie.automation.json`).
- **🎨 Entrée** (avant le circuit) : luminosité, contraste, gamma, saturation, teinte, et une
  **LUT `.cube`** (color.io, Resolve, Kdenlive…) avec son dosage. Ce qui entre colore toute la
  boucle.
- **Calques vidéo** (onglet Image) : jusqu'à 3 boucles vidéo par-dessus l'image, chacune avec
  Placement (« Dans le circuit » : les effets l'abîment aussi ; « Par-dessus » : intact), Mode
  (18 modes : Normal, Écran, Addition, Multiplier, Soustraction, Différence, Superposition,
  Lumière douce / forte, Éclaircir, Assombrir, Densité couleur − / +, Couleur, Luminosité,
  Incrustation du noir / du blanc / du vert),
  Opacité, Taille, Position, Vitesse. Opacité et Taille sont pilotables par le son. Une **barre de sélection** choisit le
  passage du fichier qui boucle (poignées fines, zone à glisser, Maj = 10× plus fin, molette =
  une image ; 20 s au plus, gardées en mémoire).
- **📼 Magnétoscope VHS** (entre le boîtier et la télé, sur le signal composite) : Usure
  (couleur qui bave et traîne, luma moins nette, souffle), Tracking (bande où l'image glisse et se déchire), Traits (les traits blancs qui défilent dans la bande),
  Drop-outs (traits blancs, ligne répétée), Instabilité (lignes qui tremblent, haut qui ondule,
  commutation des têtes). Tous pilotables par le son.
- **🎥 Caméra** (sous Video Feedback, ne compte que si Mix > 0, tout est coupé à 0) :
  - *Cadrage* : Pan, Tilt, Zoom, Rotation continus (en plus de la position choisie), Dérive
    (le pied glisse tout seul), Secousses (caméra à l'épaule, à brancher sur les attaques) ;
  - *Couleur par passage* : Teinte / passage (tunnels arc-en-ciel), Négatif (inverse la lumière) ;
  - *Tube* : Traînées (queues de comète), Halo des hautes lumières, Marquage (image fantôme) ;
  - *Optique* : Fisheye, Vignettage, Aberration, Moiré (la caméra voit les lignes de l'écran) ;
  - *Miroirs* : gauche/droite, haut/bas, quatre miroirs, kaléidoscope 6 ou 8.
  Tous ces boutons sont pilotables par le son.
- **🎞 Sortie** : une LUT `.cube` sur l'image finale, avec son dosage.
- **Sauver… / Charger…** un réglage (JSON).
- **🎵 Réaction au son** : le son de la vidéo pilote les boutons. Vumètres (volume, basses,
  médiums, aigus, attaques), presets (« Pulse sur le kick », « Tunnel qui respire », « Télé qui
  décroche », « Tout réagit ») et intensité globale. Pour chaque bouton : la source qui le pousse
  et la profondeur (négative = le son le fait baisser). Le son est joué pendant l'aperçu.
- **Bandes de fréquence** : 4 bandes réglables sur un spectre en direct, comme les points d'un
  EQ : glisser un point (← → fréquence, ↑ ↓ sensibilité), molette = largeur, clic sur le fond =
  y placer le point sélectionné. Chaque bande suit le **Niveau** ou les **Attaques** et devient une
  source (Bande 1 à 4) pour n'importe quel bouton. Départ : kick, caisse claire, voix, hi-hat.

### En ligne de commande

```sh
peugeot300mille render IN.mp4 OUT.mp4 --params reglage.json [--start 12.5] [--frames 300]
peugeot300mille render IN.mp4 OUT.mp4 --automation sortie.automation.json
peugeot300mille render IN.mp4 OUT.mp4 --params r.json --lut-in look.cube --lut-out film.cube --overlay1 boucle.mp4@2.5-8
```

## Comment ça marche

| Étage | Fichier | Rôle |
|---|---|---|
| Signal | `engine/signal.rs` | géométrie NTSC, niveaux (sync 0, blanking 0,286, noir 0,3396), gabarit de synchro |
| Encodeur | `engine/encoder.rs` | RGB → YIQ, bande limitée, composite avec salve |
| Ampli | `engine/amp.rs` | clamp, enhancer (sur la luma, réjecteur de chroma), saturation douce, feedback échantillon par échantillon, ronflement 60 Hz |
| Entrée | `engine/input.rs`, `engine/lut.rs` | correcteur, LUT 3D `.cube` (trilinéaire) en entrée et en sortie |
| Sync mixer | `engine/syncmix.rs` | verrouillé → roule → monte au blanc → grain → neige → plonge au noir |
| VHS | `engine/vhs.rs` | séparation Y/C, couleur « under » (bande étroite, retard), bruit FM, base de temps, flagging, commutation des têtes, tracking, drop-outs |
| TV | `engine/tv.rs` | séparateur de synchro adaptatif, PLL H (volant), synchro V + oscillateur libre, PLL couleur, ACC, color killer, rééchantillonnage Catmull-Rom |
| Caméra | `engine/camera.rs` | flou d'objectif, pan/tilt/zoom/roll, aberration chromatique, saturation, exposition auto, grain ; filme à 384×240 |
| Son | `audio.rs` | décodage, analyse par image TV (volume, 3 bandes fixes, attaques), spectre FFT 1/12 d'octave, 4 bandes utilisateur, lecture via cpal |
| Temps | `runner.rs` | la TV tourne à 29,97 i/s quelle que soit la vidéo ; 8 images de chauffe à chaque départ |

Le rendu est déterministe : une même plage avec les mêmes réglages donne le même fichier.

## Tests

```sh
PEUGEOT300MILLE_TEST_VIDEO=/chemin/video_avec_son.mp4 cargo test --release
```

## Pas encore là

- Glitch du son piloté par le moteur (le son de sortie est recopié tel quel).
- Son d'un fichier séparé ou du micro pour piloter les effets.
- Randomizer de réglages.
- Réglages fins de l'ampli et du sync mixer pour coller au look de Yurenaki.
