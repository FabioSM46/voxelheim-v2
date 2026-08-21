# Game Design Document: Voxel Survival RPG (Minecraft x WoW)

## 1. Titolo e Visione Generale
*   **Nome del Gioco:** Voxelheim [cite: 4]
*   **Concept:** Un survival cooperativo che unisce esplorazione, terraformazione voxel e combattimento action con la progressione PvE, i ruoli e le istanze di *World of Warcraft* [cite: 4].
*   **Modalità:** Esclusivamente Survival [cite: 4].
*   **Ambientazione:** Mitologia Norrena Oscura (*Fimbulvetr*). Mondo procedurale ostile dominato da fiordi, montagne e foreste, arricchito da rovine e misteriose iscrizioni autentiche in *Younger Futhark* [cite: 4].

## 2. Architettura Tecnica
*   **Struttura:** Monorepo con architettura ibrida Client-Server [cite: 4].
*   **Backend:** Scritto in **Go** per gestire il netcode, il multithreading massivo (generazione chunk) e la logica autoritativa [cite: 4].
*   **Frontend:** Scritto in **Rust** (con ECS/WGPU) per rendering ad alte prestazioni (Greedy Meshing) e fluidità assoluta [cite: 4].
*   **Comunicazione:** *FlatBuffers* per garantire contratti di rete granitici tra client e server [cite: 4].

## 3. Combattimento e Classi
*   **Stile Action:** Combattimento diretto basato sul posizionamento (stile Minecraft). Uso di spade, scudi, archi e armi da fuoco rudimentali a ricarica lenta [cite: 4].
*   **Soft-Classing (Gear-Based):** Nessuna classe rigida iniziale. I ruoli nel party (Tank, Healer, DPS) sono determinati unicamente dall'equipaggiamento indossato (es. armatura pesante genera Aggro) [cite: 4].

## 4. Morte e Sopravvivenza (Il Loop Anti-Zerg)
*   **Penalità di Morte:** Nessuna perdita dell'inventario, ma **-20% di durabilità** a tutto l'equipaggiamento. A durabilità 0 l'oggetto diventa inutilizzabile [cite: 4].
*   **Riparazioni:** Avvengono sul campo tramite kit consumabili limitati (pietre per affilare, toppe). Finite le scorte, il giocatore deve tornare a una stazione fissa [cite: 4].
*   **Respawn:**
    *   *Open World:* Il giocatore rinasce alla propria tenda o al villaggio più vicino [cite: 4].
    *   *Nei Dungeon:* Possibilità di rialzarsi sul posto dopo un countdown di **10 secondi** (con invulnerabilità temporanea ma usura dell'equipaggiamento) [cite: 4].

## 5. Viaggio, Logistica ed Esplorazione
*   **L'Inventario:** Sistema classico a slot, senza malus di peso o lentezza nei movimenti [cite: 4].
*   **Le Tende:** Hub temporanei dispiegabili su superfici piane nel mondo selvaggio. Offrono riparo, punto di respawn ed essenziali per il riassetto dell'inventario [cite: 4].
*   **Cavalcature:** Aumentano la velocità di esplorazione, ma **non trasportano materiali edili pesanti**. Costringono le gilde a organizzare spedizioni logistiche di gruppo a piedi per fondare nuovi avamposti [cite: 4].

## 6. Dungeon e Istanze
*   **Sistema Portali:** Dungeon situati nel mondo aperto (rovine), accessibili tramite l'inserimento di specifiche *Chiavi Runiche* [cite: 4].
*   **Istanziali:** Una volta varcato il portale, la sessione è protetta e legata esclusivamente al party (*Instance Binding*) [cite: 4].
*   **Regole di Ingresso:** Nessun giocatore può entrare o rientrare nel dungeon se il party all'interno è in stato di combattimento con un boss [cite: 4].
*   **Lockout Temporali:** Reset giornaliero per i dungeon normali, reset settimanale per i Raid epici [cite: 4].

## 7. Crafting ed Economia
*   **Stazioni Avanzate:** Necessarie stazioni fisse nei villaggi (Forgia, Banco per Pelli, Tavolo delle Incisioni) per l'equipaggiamento bellico [cite: 4].
*   **Loot dei Dungeon Ibrido:**
    *   *Equipaggiamento diretto:* Adrenalina immediata, ma soggetto a usura nel tempo [cite: 4].
    *   *Reagenti rari/Rune:* Costringono i giocatori a tornare alle stazioni di crafting [cite: 4].
    *   *Progetti (Blueprints):* Sbloccano permanentemente nuove ricette fisse per la gilda [cite: 4].

## 8. Progressione del Personaggio
*   **Sistema a Livelli:** 30 Livelli totali di base [cite: 4].
*   **Statistiche:** L'aumento di livello funge da moltiplicatore per le statistiche base (Salute, Energia, Forza), potenziando l'efficacia dell'equipaggiamento [cite: 4].
*   **Acquisizione EXP:** L'esperienza si ottiene tramite gameplay attivo: uccidendo mob (da animali per il cibo a boss dei dungeon), raccogliendo risorse specifiche (tagliare legna, estrarre pietre preziose o fiori) e craftando oggetti alle stazioni. Nessuna EXP è assegnata per la distruzione generica di voxel (es. scavare terra e pietra comune) [cite: 4].

## 9. Costruzione e Protezione del Mondo (Land Claim)
*   **Mondo Malleabile:** L'open world voxel è interamente distruttibile e scavabile per permettere la terraformazione e la raccolta delle risorse [cite: 4].
*   **Edilizia Modulare:** Le basi non si costruiscono impilando singoli blocchi, ma assemblando moduli prefabbricati (fondamenta, pareti, tetti) per garantire un'estetica norrena maestosa e coerente [cite: 4].
*   **Pietre Runiche (Ward System):** Le gilde proteggono i propri villaggi piazzando un monolite attivato tramite incisioni runiche, generando un'area inviolabile (no griefing, no furti) [cite: 4].
*   **La Tempesta del Fimbulvetr (Regenerazione del Mondo):** Una volta a settimana, in concomitanza con il reset dei Raid, una mostruosa tempesta di neve spazza il server. Tutti i chunk non protetti dalle Pietre Runiche vengono rigenerati al loro stato procedurale originale, ripristinando minerali, foreste e riparando il terreno distrutto [cite: 4].
