import random

def modify_file(input_file, percentage):
    # Leggi il file in una lista di righe
    with open(input_file, 'r') as f:
        lines = f.readlines()

    num_rows = len(lines)

    # Per ogni riga, decide se riposizionarla
    for i in range(num_rows):
        if random.randint(1, 100) <= percentage:  # Decidi in base alla percentuale
            # Scegli un numero di righe da 10 a 60 indietro
            if i >= 10:
                new_position = max(0, i - random.randint(10, 60))
                line_to_move = lines.pop(i)
                lines.insert(new_position, line_to_move)

    # Sovrascrivi il file con le righe modificate
    with open(input_file, 'w') as f:
        f.writelines(lines)

# Esempio di utilizzo:
input_file = 'dataset.txt'  # Nome del file generato in precedenza
percentage = 50  # Percentuale da 1 a 100
modify_file(input_file, percentage)
