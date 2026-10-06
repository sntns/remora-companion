# Spécification : station de provisioning (claim au premier boot)

Statut : proposition v3, intègre les décisions du 2026-10-06
Dépôts concernés : **remora-etcher** (station), remora-edge (client de claim et composant LED), meta-remora / seniortech-os (image)

## 1. Contexte et objectif

Les hubs (hub-v1 et hub-v2, tous deux en production) sont produits en série par **duplication de carte SD**. Toutes les cartes sont des copies binaires d'une même image maître, et aucune étape par appareil n'est possible au moment de l'écriture.

Objectif : une carte clonée, insérée dans un hub et mise sous tension sur le réseau d'atelier, obtient seule une identité unique. Un opérateur colle la bonne étiquette sur le bon hub, avec un **détrompage**. Le hub s'enrôle ensuite normalement sur sntns-platform. L'image ne contient aucun credential capable de créer des devices.

Cadence visée : une dizaine de hubs allumés en même temps. L'émission des identités est automatique et parallèle. L'étiquetage est **séquentiel** : un seul hub à la fois a sa LED fixe.

```
 hub (image clonée)               station (remora-etcher)                 sntns-platform
 ──────────────────               ───────────────────────                 ──────────────
 first-boot: machine-id, clé SSH,
 hostname provisoire (MAC eth /
 serial BSP)
 accessd: pas de factory yaml
 → mode claim       LED: recherche
 clé P-256 + CSR
 ── POST /v1/claims ────────────▶ approbation auto (politique par board)
                                  ── CreateFactoryDevice(CSR, params) ─▶
                                  ◀─ IDevID, CA, server CA, url, serial ─
 ◀─ issued, label=queued ───────  issued.d/*          file d'étiquetage
 vérifie la réponse   LED: en file
 (poll 1 s)
 ◀─ label=active ───────────────  hub en tête de file → label.d/* (impression)
 LED: FIXE                        l'opérateur colle l'étiquette sur le hub
                                  à LED fixe et scanne l'étiquette collée
                                  scan = serial attendu ? sinon refus
 ◀─ label=labelled ─────────────  labelled.d/*        → hub suivant
 écrit l'identity squashfs   LED: éteinte
 ── POST /v1/claims/{id}/ack ───▶ installed.d/*
 reboot → bootstrap normal ──────────────────────────────────────────────▶
```

Le **point de commit** est la validation de l'étiquette. Le hub n'écrit son identité qu'après le scan validé. Une coupure à n'importe quel moment se rattrape grâce au même `claim_id` (§4.6).

### Ce qui existe déjà et qu'on réutilise

- La plateforme accepte une **CSR** (`DeviceServiceCreateFactoryDeviceRequest.certificate_signing_request`, sujet ignoré). La clé privée ne quitte donc jamais le hub.
- `FactoryProvisioningAdapter::provision(context, serial, csr_der)` (`components/factory/src/adapter/provisioning/service.rs`) fait déjà l'appel gRPC.
- Les credentials de contexte sont statiques (pas de token à rafraîchir), donc utilisables par une station longue durée.
- Côté hub :
  - remora-accessd relit le fichier factory à chaque tentative ;
  - `remoractl identity` sait modifier le squashfs ;
  - `ManagerService::reboot` existe ;
  - `remora-boot` exécute `first-boot-after-install.d/*` avant le montage de l'identité ;
  - la LED de hub-v2 (`green:heartbeat`, PWM, trigger `pattern` activé) est pilotée aujourd'hui par le script `remora-led-heartbeat`.

### Constats sur l'image actuelle (bloquants pour le clonage)

- La distro utilise par défaut `hub-bootstrap-localdev` : **une identité fixe intégrée à l'image** (clé SSH, `remora-factory.yaml`, `remora-operational.yaml`).
- `hub-bootstrap.bb` (production) livre une identité vide. Il a deux bugs :
  - `special_boot` est écrit dans `do_compile`, donc perdu ;
  - `RCONFLICTS` est assigné deux fois.
- Rien ne génère au premier boot la clé SSH (désactivée dans `openssh_%.bbappend`), le machine-id ni le hostname.
- La MAC `bat0` du mesh = SHA256(hostname). Des clones de même hostname **entrent en collision sur le mesh**.

## 2. Réseau d'atelier

- **Transport** : HTTP/JSON sur IP.
- **Lien** : Wi-Fi client sur un SSID d'atelier dédié, via `wlan0` (brcmfmac) sur hub-v1 et sur hub-v2. Sur hub-v2, le conflit BLE/Wi-Fi ne gêne pas : en mode claim, le BLE n'annonce rien. Sur hub-v1, l'Ethernet branché sur le même réseau fonctionne aussi.
- **Point d'accès recommandé** : le **hotspot du portable station** (Linux NetworkManager : station en 10.42.0.1 ; Windows : 192.168.137.1).
- **Découverte de la station**, sans mDNS (absent de l'image), dans l'ordre :
  1. URLs explicites de la config du hub ;
  2. `http://<passerelle par défaut>:8484` ;
  3. sondage de chaque candidat par `GET /v1/hello`.
- **Une seule station par réseau d'atelier** : deux stations ont deux journaux distincts et pourraient émettre deux serials pour un même hub.
- **SSID et PSK** sont intégrés à l'image. Le profil réseau n'est actif **qu'en mode claim**. Une fuite du PSK n'expose que le réseau d'atelier.

## 3. Protocole hub ↔ station (contrat v1)

- HTTP/1.1 en clair, port 8484 par défaut, JSON, binaires en base64 standard.
- Le TLS est inutile : tout ce qui circule est public, et le hub vérifie l'identité reçue contre des ancres intégrées à l'image (§5.4).
- Corps de requête limités à **64 Kio** (une CSR P-256 en base64 fait moins de 1 Kio) ; au-delà : `413`.
- Les corps sont définis une seule fois, dans `components/station-protocol` (`remora-station-protocol`), partagé par le serveur et le client.

### Erreurs

Toute réponse d'erreur de la station a pour corps :

```json
{ "error": "<message lisible>", "code": "<code kebab-case>", "retry_after": 5 }
```

`error` sert aux journaux ; le hub décide sur `code`, stable dans la v1. `retry_after` (secondes, aussi en en-tête `Retry-After`) n'accompagne que les `503`.

| Statut | `code` | Routes | Sens, et réaction du hub |
|---|---|---|---|
| `400` | `invalid-csr` | claims | CSR illisible, clé non P-256 ou auto-signature invalide. **Seul cas où le hub régénère sa clé.** |
| `400` | `invalid-hardware` | claims | Un champ `hardware`/`image` refusé (nommé dans `error`, voir ci-dessous), ou absent alors que le `device-name` du board en a besoin. |
| `400` | `invalid-request` | claims, ack | JSON invalide, champ obligatoire manquant (dont `hardware.board` vide), base64 invalide. |
| `413` | `payload-too-large` | claims, ack | Corps de plus de 64 Kio. |
| `403` | `unknown-board` | claims | Board non configuré sur cette station. |
| `403` | `quota-exceeded` | claims | `max-claims` atteint pour cette session. |
| `403` | `refused` | claims | Refus plateforme (permission, policy). |
| `403` | `missing-access-url` | claims | La plateforme n'a pas d'URL d'accès pour les devices de ce déploiement. |
| `409` | `already-exists` | claims | `device_name` explicite déjà existant (§4.2) ; LED erreur. |
| `409` | `not-labelled` | ack | Ack `installed` avant la validation de l'étiquette. |
| `404` | `unknown-claim` | status, ack | Claim inconnu de cette station. |
| `503` | `unavailable` | claims | Plateforme injoignable, ou credentials de la station refusés ; réessayer après `retry_after`. |
| `503` | `journal-unavailable` | claims | Le journal ne peut pas être écrit (§4.6) ; réessayer après `retry_after`. |
| `503` | `not-started` | toutes | Station pas encore prête ; réessayer après `retry_after`. |
| `500` | `internal` | toutes | Autre erreur. |

### Ce que le hub dit de lui-même

Tout ce que le hub déclare atteint le terminal de l'opérateur, l'environnement des hooks, le journal et les gabarits `device-name`. La station le valide donc avant tout (`400 invalid-hardware` sinon) :

- une chaîne vide vaut absence, partout sauf `board` (le hub envoie `""` pour ce qu'il ne sait pas lire) ;
- `board` (non vide), `temp_hostname`, `eth_mac`, `bsp_serial`, `machine_id`, `image.version`, `image.compatible` et les valeurs de `macs` : au plus 64 caractères parmi `[A-Za-z0-9:._+-]` ;
- `macs` : au plus 32 entrées, clés (noms d'interface) d'au plus 16 caractères parmi `[A-Za-z0-9._-]`.

La `reason` d'un ack `failed` n'est jamais refusée : les caractères de contrôle (sauts de ligne compris) deviennent des espaces, les espaces sont fusionnés, et elle est tronquée à 512 caractères.

### `GET /v1/hello`

```json
{ "service": "remora-station", "protocol": 1, "environment": "eu2" }
```

### `POST /v1/claims`

```json
{
  "csr": "<base64 DER PKCS#10, clé P-256>",
  "hardware": {
    "board": "hub-v2",
    "temp_hostname": "e2b4a1c09f13",
    "eth_mac": "e2:b4:a1:c0:9f:13",
    "bsp_serial": "c3d2…",
    "machine_id": "1abf02e5bed34d63a097f3f38f0f6408",
    "macs": { "wlan0": "…", "wlan1": "…" }
  },
  "image": { "version": "1.4.0", "compatible": "v2" }
}
```

**`claim_id`** = SHA-256 hexadécimal de la `SubjectPublicKeyInfo` de la CSR. Il est déterministe : un hub qui réessaie avec la même clé retombe sur le même claim. **Un retry n'alloue jamais un deuxième serial.**

`200` avec un `ClaimStatus` : claim connu ou nouvellement émis. Sinon, une erreur du tableau ci-dessus (`invalid-*`, `payload-too-large`, `unknown-board`, `quota-exceeded`, `refused`, `missing-access-url`, `already-exists`, `unavailable`, `journal-unavailable`).

### `GET /v1/claims/{claim_id}`

Le hub interroge cette route **toutes les secondes** tant qu'il n'a pas reçu `labelled`. Ce poll sert aussi de signal de présence à la station (§4.5).

`ClaimStatus` :

```json
{
  "claim_id": "…",
  "state": "issued",
  "label": "queued",
  "queue_position": 3,
  "identity": {
    "serial_number": "1H7Z",
    "factory_device_name": "urn:sntns:…:device:1H7Z",
    "certificate": "<base64 DER IDevID>",
    "certificate_authority": "<base64 DER>",
    "server_certificate_authority": "<base64 DER>",
    "key_id": "<certificate_reference.urn>",
    "access_url": "https://…"
  }
}
```

- `state` vaut `issued`, `installed` ou `failed`. `identity` est présent dès `issued`.
- `label` vaut :
  - `queued` : en attente de son tour ;
  - `active` : c'est ce hub qu'on étiquette, LED fixe ;
  - `labelled` : étiquette validée, le hub peut écrire son identité et redémarrer.

### `POST /v1/claims/{claim_id}/ack`

```json
{ "state": "installed" }
{ "state": "failed", "reason": "certificate does not chain to the baked factory authority" }
```

- `installed` : le hub l'envoie après l'écriture du squashfs et avant le reboot, au mieux. Une perte est sans conséquence. **Refusé (`409 not-labelled`) tant que l'étiquette n'est pas validée** : le `claim_id` circule en clair à chaque poll, et seul le point de commit autorise un hub à se dire installé (et à déclencher `installed.d`).
- `failed` : accepté **dans tout état**, puisque le hub vérifie son identité dès réception (§5.2, étape 4). Il est journalisé, déclenche `failed.d` et **sort le claim de la file d'étiquetage** ; s'il était actif, le hub suivant devient actif. Un claim `failed` n'est plus jamais activé.
- Après un ack, `label` reste cohérent : `labelled` si l'étiquette avait été validée, sinon `queued` sans `queue_position` (hors file).
- Un ack répété (réponse perdue) renvoie le même `ClaimStatus` sans relancer de hook. `installed` est définitif ; `failed` peut encore devenir `installed` une fois le claim `labelled` (le hub a finalement écrit son identité).
- `200` avec le `ClaimStatus`, ou `400 invalid-request`, `404 unknown-claim`, `409 not-labelled`.

## 4. remora-etcher : vertical `station` (périmètre principal)

### 4.1 Lancement

```
remora-etcher -c factory station serve --config station.yaml
```

Toutes les clés du fichier ont un équivalent en option CLI (`--listen`, `--journal`, `--hooks`, `--max-claims`…). `-c/--context` reste avant la sous-commande, comme partout.

```yaml
# station.yaml
listen: 0.0.0.0:8484
journal: ./station.jsonl          # rechargé au démarrage
hooks: ./station.d                # répertoires <évènement>.d/ (§4.4)
max-claims: 50                    # optionnel : quota d'émissions par session
confirm: scan                     # scan | key (§4.5)
presence-timeout: 10s             # hub actif sans poll depuis ce délai → remis en file

boards:                           # boards acceptés ; tout autre board → 403
  hub-v2:
    create-factory-device:
      serial-number-policy: hubs-v2
  hub-v1:
    create-factory-device:
      serial-number-policy: hubs-v1
  # variante : device_name explicite au lieu d'une policy
  # hub-v1:
  #   create-factory-device:
  #     device-name: "{bsp_serial}"
```

- **Approbation automatique** : tout claim d'un board configuré est émis immédiatement. Le contrôle humain se fait à l'étiquetage (§4.5).
- **Paramètres de `CreateFactoryDevice`, configurables par board** :
  - `serial-number-policy` **ou** `device-name`. Exactement l'un des deux, comme `DeviceSerial::from_parts`.
  - `device-name` est un gabarit sur les champs `hardware` du claim (`{bsp_serial}`, `{eth_mac}`, `{temp_hostname}`, `{board}`).
  - `force` n'est **pas** exposé : le SAV est hors périmètre. Un `device-name` déjà existant renvoie 409, journalisé.
- **Contexte** : il doit avoir `remora::create-factory-device`, plus `remora::use-serial-number-policy` sur chaque policy utilisée. On recommande un **contexte `access-key` dédié**, avec un rôle limité à ces permissions.
- **Validation au démarrage**, avant d'écouter :
  - le contexte se résout et est connecté : la station appelle `whoami` sur la plateforme, donc des credentials révoqués l'empêchent de démarrer (et, en cours de route, renvoient `503 unavailable` aux hubs avec une alerte à l'opérateur) ;
  - le gabarit de chaque board est valide ;
  - le répertoire de hooks est lisible.
- **Sorties**, selon les conventions du dépôt :
  - **stdout** : un serial par ligne à chaque émission ;
  - **stderr** (via `remora-tui`) : le tableau de bord (§4.5).

Commande de test, pour les tests d'intégration et hub-virtual :

```
remora-etcher station simulate --url http://127.0.0.1:8484 --board hub-virtual [--output remora-factory.yaml]
```

Elle joue un hub complet : clé, claim, poll, LED simulée sur stderr, écriture du `remora-factory.yaml` au format de `factory provision`, et ack. C'est un transport du vertical `claim` (§4.3), qui porte la logique côté hub : réessais sur `503` (après `retry_after`) et sur erreur réseau, abandon sur tout autre refus.

### 4.2 Évolution du vertical `factory`

```rust
// components/factory/src/application/service.rs
async fn provision_csr(
    &self,
    over: Option<ContextOverride>,
    serial: DeviceSerial,
    csr_der: &[u8],
) -> Result<ProvisionedIdentity>;
```

- La CSR est validée **avant** l'appel plateforme : PKCS#10, P-256, auto-signature. Sinon, `Error::InvalidCsr` → 400.
- La méthode réutilise `FactoryProvisioningAdapter::provision` et la résolution d'`access_url` existante.
- `provision` (existant) se réécrit au-dessus de `provision_csr` + `DeviceKeyAdapter` + `CredentialWriterAdapter`, sans changement de comportement.
- Vérification de la CSR avec `x509-cert` + `p256`.

### 4.3 Découpage en crates (conventions CLAUDE.md, gabarit `components/flash*`)

| Crate | Contenu |
|---|---|
| `components/station` (`remora-station`) | Modèle : `ClaimId`, `ClaimRequest`, `HardwareInfo`, `ImageInfo`, `ClaimState`, `LabelState`, `StationConfig`, `BoardPolicy`, `HookEvent`. Ports : `JournalAdapter`, `HookRunnerAdapter`, `OperatorAdapter` (file d'étiquetage : afficher le hub actif, lire une confirmation ou une commande). `StationServiceInterface { hello, submit, status, ack }`. Aucune I/O. |
| `station-application` | `StationControllerImpl` : injecte `ContextService`, `FactoryService`, et les trois ports. Gère la machine d'états, l'idempotence, le quota, la file d'étiquetage, la présence et le lancement des hooks. |
| `station-adapter-jsonl` | Journal JSONL (append + fsync, rechargement). |
| `station-adapter-hooks-process` | Exécution des `<évènement>.d/*` (§4.4). |
| `station-adapter-operator-tui` | Tableau de bord (`remora-tui`) ; affichage seulement, les caractères de contrôle retirés. |
| `station-protocol` | Les corps JSON du §3, les codes d'erreur et leurs conversions, partagés par le serveur et le client. |
| `station-application-transport-http` | Routeur axum 0.8 (déjà dans Cargo.lock via tonic) qui projette le §3 sur `StationService`. Aucune logique métier. |
| `station-application-transport-cli` | `station serve` (lit le clavier/la douchette, horloge de la file, Ctrl-C, arrêt propre) et `station simulate`. |
| `components/claim` (`remora-claim`) | Côté hub : `ClaimPlan`, `ClaimOutcome` (sur les types du modèle `station`), port `StationClientAdapter` (hello, claim, status, ack), `ClaimServiceInterface`. |
| `claim-adapter-http` | Client HTTP du §3, avec timeouts de connexion et de requête. |
| `claim-application` | `ClaimControllerImpl` : injecte `StationClientAdapterService` et `FactoryService` (clé du device, écriture du `remora-factory.yaml`) ; réessais, poll, LED rapportée en progression. |

`StationServiceInterface` expose `start`, `hello`, `submit`, `status`, `ack`, plus `handle` (une saisie de l'opérateur), `tick` (l'horloge de la file : présence, activation) et `shutdown` (arrêt propre, §4.4). La boucle de la console vit dans le transport.

Câblage dans `containers/remora-etcher/src/bootstrap.rs`. Nouvelles dépendances directes : `axum`, `x509-cert`. Pas de mDNS.

### 4.4 Hooks externes (`*.d`)

Arborescence sous `hooks:` (par défaut `./station.d`) :

| Répertoire | Déclencheur | Bloquant | Usage typique |
|---|---|---|---|
| `issued.d/` | identité émise par la plateforme | non | enregistrement ERP / registry |
| `label.d/` | le hub devient `active` (LED fixe) | **oui** | impression de l'étiquette |
| `labelled.d/` | scan validé | non | traçabilité, compteur de lot |
| `installed.d/` | ack `installed` du hub | non | clôture dans le registry |
| `failed.d/` | ack `failed`, 409, ou erreur de vérification | non | alerte |

**Exécution**, à la manière de `run-parts` :

- Les fichiers du répertoire sont lancés **par ordre lexical**, l'un après l'autre.
- Sous Unix : seuls les fichiers exécutables. Sous Windows : `.exe`, `.cmd`, `.bat` et `.ps1`, ce dernier via `powershell -File`.
- Sont ignorés : les fichiers cachés, les `*~` et les `*.disabled`.
- Timeout par script : 60 s par défaut (`hook-timeout`).
- stdout et stderr de chaque script sont capturés dans le journal et affichés dans le tableau de bord.
- Un échec dans un répertoire non bloquant est journalisé et signalé, mais n'arrête pas le flux.
- Un échec dans `label.d` bloque la validation du hub actif jusqu'à une réimpression réussie (commande `r`) ou un contournement explicite (commande `f`, journalisé).
- **À l'arrêt** (`q`, Ctrl-C), une fois le serveur HTTP arrêté : plus aucun hook ne démarre, ceux en cours ont `hook-timeout` pour finir (leur sortie est journalisée), les autres sont tués (signalé à l'opérateur), puis le journal est vidé sur disque.

**Paramètres** passés à chaque script :

- `argv[1]` : nom de l'évènement (`issued`, `label`…).
- **stdin** : l'enregistrement complet du claim en JSON (requête, identité émise sans les certificats, états).
- **Environnement** :

| Variable | Exemple |
|---|---|
| `REMORA_EVENT` | `label` |
| `REMORA_CLAIM_ID` | `9f3c…` |
| `REMORA_SERIAL` | `1H7Z` |
| `REMORA_FACTORY_DEVICE_NAME` | `urn:sntns:…:device:1H7Z` |
| `REMORA_HOSTNAME` | `1H7Z` (hostname définitif = serial tel quel) |
| `REMORA_TEMP_HOSTNAME` | `e2b4a1c09f13` |
| `REMORA_BOARD` | `hub-v2` (type de machine) |
| `REMORA_ETH_MAC` | `e2:b4:a1:c0:9f:13` (vide si absent) |
| `REMORA_BSP_SERIAL` | `c3d2…` |
| `REMORA_MACHINE_ID` | `1abf…` |
| `REMORA_IMAGE_VERSION` / `REMORA_IMAGE_COMPATIBLE` | `1.4.0` / `v2` |
| `REMORA_ACCESS_URL` | `https://…` |
| `REMORA_SERIAL_POLICY` / `REMORA_DEVICE_NAME_TEMPLATE` | selon le board |
| `REMORA_CONTEXT` | `factory` |
| `REMORA_ATTEMPT` | `1` (`2`, `3`… en cas de réimpression ou de reprise) |
| `REMORA_JOURNAL` | chemin du journal |

Le contenu de l'étiquette relève du script d'impression. Il doit contenir **le serial sous forme de code-barres** (DataMatrix ou QR), qui sert au détrompage.

### 4.5 File d'étiquetage et détrompage

**File :**

- Les claims `issued` entrent dans une file FIFO, triée par heure d'émission.
- **Un seul hub est `active` à la fois** : la tête de file. Il voit `label=active` à son prochain poll (≤ 1 s) et passe sa LED en **fixe**. Les autres restent en motif « en file ».

**Détrompage**, en quatre verrous :

1. **Unicité visuelle** : la station n'active jamais deux hubs. Le hub à étiqueter est le seul dont la LED est fixe.
2. **Vérification par scan** (`confirm: scan`, par défaut) : l'opérateur colle l'étiquette sur le hub à LED fixe, puis **scanne l'étiquette collée**. La douchette est vue comme un clavier (ligne + Entrée). La station compare le scan au serial du hub actif :
   - **identique** : `label=labelled`, puis le hub suivant devient actif ;
   - **différent** : refus avec alerte sonore (BEL) et message rouge. Le hub reste actif, rien n'est validé, et l'évènement est journalisé (`label-mismatch`).
3. **Confirmation visuelle** : à la validation, la LED du hub étiqueté **s'éteint** (§5.3) et une LED fixe apparaît sur le hub suivant. L'opérateur voit s'éteindre la LED du hub qu'il vient d'étiqueter. Si c'est un autre hub qui change d'état, l'erreur est immédiatement visible.
4. **Hostname définitif = serial** : après reboot, le hub s'annonce sous le serial imprimé. Une vérification a posteriori sur la plateforme (`rmra`) reste possible.

`confirm: key` (Entrée seule, sans scan) existe pour la mise au point. Il est déconseillé en production et signalé comme tel au démarrage.

**Commandes clavier** sur le hub actif :

| Touche | Effet |
|---|---|
| `r` | réimprimer (relance `label.d`, `REMORA_ATTEMPT+1`) |
| `s` | passer : le hub actif retourne en fin de file, sa LED repasse en « en file » |
| `f` | forcer la validation malgré un échec de `label.d` (le scan reste exigé) |
| `q` | quitter proprement |

**Présence :** si le hub actif n'a pas interrogé la station depuis `presence-timeout` (débranché, crash), la station le remet en fin de file (`label-lost`) et active le suivant. S'il revient, il reprend sa place dans la file avec le même `claim_id`.

**Tableau de bord** (stderr) :

- compteurs : en recherche, émis, en file, étiquetés, installés, en échec ;
- ligne du hub actif : serial, board, hostname provisoire, MAC ;
- derniers évènements des hooks.

### 4.6 Journal et idempotence

Une ligne JSON par transition :

```json
{"ts":"…","claim_id":"…","event":"received","board":"hub-v2","temp_hostname":"e2b4a1c09f13","eth_mac":"…","bsp_serial":"…","machine_id":"…","image":"1.4.0"}
{"ts":"…","claim_id":"…","event":"issued","serial_number":"1H7Z","factory_device_name":"…","key_id":"…","context":"factory","identity":{…}}
{"ts":"…","claim_id":"…","event":"hook","hook":"label.d/10-print","exit":0,"attempt":1}
{"ts":"…","claim_id":"…","event":"label-active"}
{"ts":"…","claim_id":"…","event":"label-mismatch","scanned":"1H8A"}
{"ts":"…","claim_id":"…","event":"labelled","scanned":"1H7Z"}
{"ts":"…","claim_id":"…","event":"installed"}
```

Les transitions sont écrites dans l'ordre par un unique écrivain, hors du verrou d'état : un poll n'attend jamais un `fsync`. Seuls `received` et `issued` sont attendus avant de répondre au hub. Si `issued` ne peut pas être écrit, l'identité est **retenue** en mémoire : le hub reçoit `503 journal-unavailable`, l'opérateur est alerté, et le retry du hub est servi depuis la mémoire une fois le journal réinscriptible -- jamais par un deuxième appel plateforme. Un `received` impossible à écrire refuse de même tout nouveau claim.

Au démarrage, la station recharge le journal. Les règles d'idempotence couvrent toutes les coupures :

| Situation | Comportement |
|---|---|
| Le hub réessaie (réponse perdue, reboot avant la fin du claim) | Même clé, donc même `claim_id`. Un claim déjà `issued` reçoit **la même identité, sans appel plateforme**. |
| La station redémarre | Les claims `issued` non étiquetés reviennent en file ; les `labelled` restent `labelled`. |
| Le hub est coupé avant la validation de l'étiquette | Il n'a rien écrit. Au reboot, même `claim_id` : il revient en file, `REMORA_ATTEMPT+1` (l'étiquette déjà imprimée peut être réutilisée ; les hooks savent qu'il s'agit d'une reprise). |
| Le hub est coupé après validation, avant l'écriture | Au reboot, même `claim_id`, état `labelled` : il écrit directement, sans nouvel étiquetage. |
| Le hub est coupé après l'écriture, avant l'ack | Il démarre enrôlable. Le journal reste à `labelled`, ce qui est acceptable. |
| Deux requêtes concurrentes pour le même claim | Un verrou par `claim_id` garantit un seul appel plateforme. |

Ce journal est aussi le registre de production : serial ↔ MAC ↔ serial BSP ↔ machine-id ↔ date ↔ contexte.

### 4.7 Tests

- Selon les règles du dépôt : pas de mocks. La plateforme est le fake gateway in-process (`test-gateway`, qui implémente `CreateFactoryDevice`), avec un vrai client HTTP et de vrais scripts de hook dans un répertoire temporaire.
- Cas à couvrir :
  - 10 claims concurrents → 10 serials ; un seul `active` à la fois ; ordre FIFO ;
  - scan correct → `labelled` → suivant ;
  - scan incorrect → refus, aucun changement d'état ;
  - retry avec la même clé, et redémarrage de la station sur le même journal → même serial, un seul appel plateforme ;
  - hub silencieux > `presence-timeout` → remis en file ;
  - échec de `label.d` → validation bloquée, `r` relance, `f` force ;
  - ordre lexical, timeout et environnement des hooks ;
  - board non configuré → 403 ; quota → 403 ; `device-name` existant → 409 ; plateforme `Unavailable` → 503 ; CSR invalide → 400 ; chaque code du §3 ;
  - ack `installed` avant étiquetage → 409 `not-labelled` ; ack `failed` avant étiquetage → accepté, hors file ;
  - valeurs réelles et chaînes vides acceptées, chaque limite de champ refusée (`invalid-hardware`), corps trop gros (413) ;
  - `issued` impossible à journaliser → 503, aucun deuxième appel plateforme ;
  - arrêt avec des hooks en cours : attendus jusqu'à `hook-timeout`, tués au-delà ;
  - `simulate` produit un `remora-factory.yaml` lisible par le `FactoryIdentity` de remora-edge.

### 4.8 Sécurité

- Les credentials plateforme restent sur le portable station (contexte 0600).
- Pendant qu'une station tourne, quiconque sur le réseau d'atelier peut obtenir une identité. Les limites sont :
  - les boards configurés ;
  - `max-claims` ;
  - le journal ;
  - la révocation plateforme.

  Une identité émise mais jamais étiquetée reste visible dans le journal.
- La station écoute par défaut sur toutes les interfaces, ce que le hotspot impose. À lancer uniquement sur le réseau d'atelier.

## 5. remora-edge (spec compagnon)

### 5.1 Bibliothèque d'identity squashfs

- Extraire la logique de `containers/remorad/src/identity.rs` (`mutate`, `-mem 16M`, `.bak`, rename atomique) dans un composant (`components/identity-store` + adapter squashfs-tools), avec des erreurs `error-stack`.
- `remoractl identity` devient un transport de ce composant.
- Il faut une **mutation multi-fichiers atomique** : `remora-factory.yaml` (0600) **et** `hostname` en une passe.

### 5.2 Mode claim dans remora-accessd

Il est actif si `claim.enabled` et que `factory-identity-path` est absent.

```yaml
claim:
  enabled: true
  station-urls: []
  station-from-gateway: true
  port: 8484
  poll-interval: 1s
  network-profile: provisioning
  pending-key-path: /mnt/datafs/remora/claim/key.pem          # ext4, 0600
  factory-authority: /usr/share/remora/claim/factory-ca.pem   # ancres intégrées, obligatoires en prod
  server-authority: /usr/share/remora/claim/server-ca.pem
  access-url-prefix: "https://…"                              # optionnel
  board: hub-v2                                               # fourni par le BSP
  hardware-path: /run/remora/identity/hardware.json           # écrit par le hook first-boot (§6.2)
```

Déroulé :

1. Activer le profil réseau `provisioning`. LED : `claim-searching`.
2. Charger ou générer la clé P-256 et la persister **avant** tout envoi, pour que le `claim_id` reste stable. Construire la CSR.
3. Découvrir la station, puis faire `POST /v1/claims`. Réessayer avec backoff sur 503 (après `retry_after`) et sur erreur réseau. Ne régénérer la clé que sur `400 invalid-csr` (§3, Erreurs).
4. **Vérifier** l'identité reçue :
   - la clé publique du certificat est celle du hub ;
   - la signature est faite par `certificate_authority` ;
   - les deux CA sont **identiques octet pour octet** aux ancres intégrées ;
   - le CN est non vide ;
   - le préfixe d'`access_url` est respecté.

   En cas d'échec : ack `failed` (accepté dans tout état : le claim sort de la file), LED `claim-error`, la clé est conservée, nouvel essai plus tard.
5. Interroger la station toutes les secondes. La LED suit `label` : `queued` → `claim-queued`, `active` → `claim-active` (fixe).
6. Sur `labelled` :
   - LED `claim-done` (éteinte) ;
   - écriture atomique dans l'identity squashfs de `remora-factory.yaml` (format exact de `factory-adapter-local/src/yaml.rs`) et de `hostname` = **serial tel quel** (même casse, sans transformation) ;
   - ack `installed` ;
   - restauration du profil réseau ;
   - suppression de `pending-key-path` ;
   - **reboot** (`ManagerService::reboot`). Il est interdit de remonter l'identité à chaud.
7. Un 409, un 403 ou un `400 invalid-hardware` met la LED en `claim-error`, avec un nouvel essai lent (le temps que l'opérateur corrige la config de la station).

### 5.3 Nouveau composant `led`

Il sert à piloter la LED d'état de façon générique (états nommés, arbitrage entre clients), et pas seulement pour le claim.

| Crate | Contenu |
|---|---|
| `components/led` | Modèle : `LedRole` (nom de rôle déclaré par le BSP), `LedState` (nom → comportement par rôle), `Behaviour { Fixed(level), Pattern(steps), Trigger(name) }`, `Request { owner, state, priority }`. Ports : `LedAdapter`. `LedServiceInterface { set(owner, state, priority), clear(owner), current() }`. |
| `led-adapter-sysfs` | `/sys/class/leds/<name>` : trigger `pattern` (motifs), `none` + `brightness` (fixe ou éteint), ou retour à un trigger noyau nommé. Niveaux 0-255 ramenés au `max_brightness` réel (1 sur une LED binaire). Repli sur le trigger `timer` si `pattern` est absent du noyau. |
| `led-application` | Arbitrage : la requête de plus haute priorité gagne ; quand elle est relâchée, l'état précédent revient. Si un client D-Bus disparaît, ses requêtes sont libérées. L'état par défaut est `normal` (heartbeat). |
| `led-application-transport-dbus` | `io.rmra.led` (`Set`, `Clear`, `Current`). |
| `led-application-transport-cli` | `remora-ledctl set claim-active`, `remora-ledctl clear`, `remora-ledctl status`. |
| `containers/remora-ledd` | Daemon. Configuration par BSP. |

Le matériel diffère d'une carte à l'autre :

| Board | LEDs | Luminosité |
|---|---|---|
| hub-v2 (Rock S0) | 1 LED verte, `green:heartbeat` | PWM, 0 à 255 |
| hub-v1 (RPi3 B+) | 2 LEDs : ACT (verte) et PWR (rouge) | **binaire** (marche/arrêt), noms sysfs à relever (§6.3) |

Le composant raisonne donc en **rôles de LED** déclarés par le BSP. Un état fixe un comportement pour chaque rôle.

Un comportement est l'un des trois cas suivants :

- `fixed: <niveau>` : allumée en continu au niveau donné ;
- `pattern: "<niveau> <ms> …"` : motif répété (trigger `pattern`) ;
- `trigger: <nom>` : rendre la LED à un trigger noyau, pour restaurer le comportement d'origine (par exemple `mmc0` pour ACT).

Les niveaux s'écrivent de 0 à 255. L'adapter les ramène au `max_brightness` réel de la LED. Sur une LED binaire, un motif ne doit utiliser que 0 et 255 : la validation de config au démarrage du daemon le refuse sinon, pour éviter qu'un dégradé ne s'affiche comme une LED allumée en continu.

Configuration hub-v2 :

```yaml
led:
  leds:
    status: green:heartbeat
  states:
    normal:          { status: { pattern: "0 0 26 150 0 150 0 100 26 150 0 150 0 700" } }  # heartbeat actuel, atténué
    claim-searching: { status: { pattern: "0 0 255 500 255 0 0 500" } }                    # clignotement lent 1 Hz
    claim-queued:    { status: { pattern: "255 0 255 100 0 0 0 1900" } }                   # éclair bref toutes les 2 s
    claim-active:    { status: { fixed: 255 } }                                            # FIXE, pleine luminosité
    claim-done:      { status: { fixed: 0 } }                                              # éteinte
    claim-error:     { status: { pattern: "255 0 255 100 0 0 0 100" } }                    # clignotement rapide 5 Hz
```

Configuration hub-v1 (noms sysfs à confirmer) :

```yaml
led:
  leds:
    act: ACT          # ou led0 selon le noyau
    pwr: PWR          # ou led1
  states:
    normal:          { act: { trigger: mmc0 }, pwr: { trigger: default-on } }  # comportement d'origine RPi
    claim-searching: { act: { pattern: "255 0 255 500 0 0 0 500" }, pwr: { fixed: 0 } }
    claim-queued:    { act: { pattern: "255 0 255 100 0 0 0 1900" }, pwr: { fixed: 0 } }
    claim-active:    { act: { fixed: 255 }, pwr: { fixed: 255 } }   # les DEUX fixes : seul état où les deux sont allumées
    claim-done:      { act: { fixed: 0 }, pwr: { fixed: 0 } }
    claim-error:     { act: { fixed: 0 }, pwr: { pattern: "255 0 255 100 0 0 0 100" } }  # rouge clignotant rapide
```

Sans PWM, hub-v1 ne peut pas jouer sur la luminosité. La lisibilité vient donc de la combinaison des deux LEDs :

- `claim-active` est le **seul** état où les deux LEDs sont fixes ;
- l'erreur est le **seul** état où la LED rouge clignote.

Une fois le mode claim terminé, l'état `normal` rend PWR à son trigger d'origine. Pendant le claim, l'indication de sous-tension de la LED PWR est perdue, ce qui est acceptable.

Règles communes :

- Ce composant **remplace** `remora-led-heartbeat` (meta-remora-rockchip) : sur hub-v2, le motif heartbeat devient l'état `normal`.
- remora-accessd prend la LED avec une priorité « claim » pendant tout le mode claim.
- Sur chaque board, `claim-active` doit être le seul état fixe et pleinement allumé, pour que l'opérateur ne le confonde avec aucun autre.

### 5.4 Pourquoi intégrer les ancres

Le transport n'est pas authentifié. Le hub vérifie donc l'identité reçue contre les CA de l'environnement cible, intégrées à l'image. **Une image maître cible un seul environnement plateforme.**

## 6. Image Yocto (spec compagnon)

1. **Identité de production** :
   - corriger `hub-bootstrap.bb` (`special_boot` dans `do_install`, `RCONFLICTS` en double) ;
   - le sélectionner pour les images maîtres (par exemple `make hub-v2 IDENTITY=production`) ;
   - ajouter une garde de build qui refuse une image « release » avec `hub-bootstrap-localdev`.
2. **Hook `first-boot-after-install.d/05-generate-identity`**, avant le montage de l'identité, donc sans reboot. Il génère ce qui manque :
   - `machine-id` ;
   - `ssh_host_ed25519_key(.pub)` (`ssh-keygen` doit être dans l'image) ;
   - **hostname provisoire**, dans cet ordre :
     1. **MAC Ethernet**, lue dans le device tree (`/proc/device-tree/aliases/ethernet0` → `local-mac-address` ou `mac-address`), puis dans `/sys/class/net/eth0/address`. Format : 12 caractères hexadécimaux en minuscules, sans séparateur, comme la convention localdev actuelle. Le DT est préféré parce que l'Ethernet USB de la RPi3 (LAN78xx) peut ne pas être encore énuméré à `local-fs.target`.
     2. À défaut, ou si le BSP déclare sa MAC non stable : le **numéro de série BSP** (`/proc/device-tree/serial-number`, ou une source surchargeable par le BSP), en minuscules.

   Le hook fait ensuite un mksquashfs `-mem 16M`. Il écrit aussi `hardware.json` (`temp_hostname`, `eth_mac`, `bsp_serial`) **dans l'identity squashfs**, lu par remora-accessd sous `/run/remora/identity/hardware.json`. Il doit survivre aux reboots, car un hub coupé avant l'étiquetage refait son claim au boot suivant, où le hook ne repasse pas.

   Variable BSP : `REMORA_TEMP_HOSTNAME_SOURCES ?= "eth-mac bsp-serial"`.
3. **Vérifications matérielles (faites le 2026-10-06 sur WMFN et R617, en lecture seule)** :
   - **hub-v2** (Rock S0) :
     - MAC `36:ae:39:96:99:97`, dérivée du cpuid par U-Boot et injectée dans le DT (`local-mac-address`) ;
     - cette MAC est *locally administered* mais stable d'un boot à l'autre : la source `eth-mac` convient ;
     - `serial-number` du DT renseigné (`f205fadcba7746a1`, identique à `serial#` d'U-Boot) ;
     - LED `green:heartbeat` en PWM 0-255, triggers `pattern` et `timer` disponibles.
   - **hub-v1** (RPi3 B+) :
     - MAC `b8:27:eb:…` (OUI Raspberry Pi), dérivée du serial OTP ;
     - LEDs `ACT` (trigger d'origine `mmc0`) et `PWR` (`default-on`) en GPIO, `max_brightness=1` ;
     - **trigger `pattern` absent** du noyau, seul `timer` est disponible. Les motifs de §5.3 pour hub-v1 sont tous des cycles marche/arrêt simples, donc exprimables en `timer` (`delay_on`/`delay_off`). L'adapter les convertit, et la validation de config refuse tout motif non convertible sur une LED sans `pattern`.
4. **Entropie (hub-v2)** : pas de HWRNG exploitable sur RK3308. Mesuré : `crng init done` à ~14,3 s, donc le hook de premier boot attend jusqu'à ~14 s via `getrandom()` bloquant, ce qui est acceptable. hub-v1 est prêt à 0 s (bcm2835-rng). Si c'est trop lent : jitterentropy, ou une seed persistée sur `/mnt/datafs`.
5. **Réseau** : un fragment `60-provisioning.yaml`, avec une connexion Wi-Fi infrastructure sur `wlan0` (SSID et PSK d'atelier) rattachée au profil `provisioning`, inactif par défaut. Il est conservé sur hub-v2 malgré le retrait de `90-local-wifi.yaml`. Sur hub-v1, l'Ethernet reste disponible.
6. **LED** :
   - recette `remora-ledd` avec un fragment de config par BSP ;
   - retrait de `remora-led-heartbeat` ;
   - fragment hub-v1 à deux LEDs (§5.3) ;
   - activation du trigger `pattern` dans le noyau hub-v1 si besoin.
7. **Claim** : installer les ancres de l'environnement (`/usr/share/remora/claim/*.pem`) et le fragment `claim:` (dont `board`) de remora-accessd.
8. Vérifier que l'image maître porte `special_boot=first-boot-after-install`.

## 7. Plan de réalisation

| Étape | Livrable | Dépend de |
|---|---|---|
| M0 | Revue et gel du contrat HTTP (§3), du format `station.yaml` et des hooks (§4.4) | — |
| M1 | remora-etcher : `provision_csr`, vertical `station` (file, détrompage, hooks, journal), `serve` + `simulate`, tests | M0 |
| M2a | remora-edge : composant `led` + `remora-ledd` | — |
| M2b | remora-edge : identity-store, mode claim dans remora-accessd | M0, M2a |
| M3 | Yocto : identité de production, hook first-boot (hostname MAC / serial BSP), profil provisioning, ancres, LED ; vérifications matérielles du §6.3 | M2a, M2b |
| M4 | E2E hub-virtual : la station sur l'hôte = la passerelle QEMU (10.0.2.2), donc `station-from-gateway` marche sans configuration | M1, M2b, M3 |
| M5 | Pilote : 10 hub-v2 + quelques hub-v1, clonés d'un même maître, allumés ensemble → serials, hostnames, clés SSH et MAC `bat0` distincts ; étiquetage scanné sans erreur ; 100 % d'enrôlements | M4 |

M1 et M2a sont parallélisables dès maintenant. M1 se teste seul avec `station simulate`.

## 8. Hors périmètre et questions ouvertes

- **Hors périmètre** : SAV et RMA (ré-émission sur un serial existant, `force`). L'idempotence couvre en revanche toutes les reprises du flux nominal (§4.6).
- **Points restants** :
  1. **Clonage, points annexes** (ticket séparé) :
     - GUID GPT et UUIDs FS identiques sur toutes les cartes ;
     - en-tête GPT de secours à 14 Go, sans extension de `data` ;
     - machine-id lu par PID1 avant le montage de l'identité.
  2. **Pré-requis production** : trousseau RAUC de production. Les bundles sont aujourd'hui signés avec `development-1`, dont la clé privée est versionnée.
- **Décidé** :
  - hostname définitif = serial tel quel ;
  - hostname provisoire = MAC Ethernet, à défaut serial BSP ;
  - approbation automatique ;
  - hooks `*.d` ;
  - hub-v1 et hub-v2 en production ;
  - SAV hors périmètre.
