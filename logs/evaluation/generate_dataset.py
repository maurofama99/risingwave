import random
from datetime import datetime, timedelta

def generate_file(num_rows, output_file):
    start_datetime = datetime(2024, 1, 1, 0, 0, 0)
    with open(output_file, 'w') as f:
        for i in range(1, num_rows + 1):
            first_field = i
            second_field = i
            third_field = random.randint(0, 9)
            timestamp = start_datetime + timedelta(seconds=i-1)
            row = f"({first_field}, {second_field}, {third_field}, '{timestamp.strftime('%Y-%m-%d %H:%M:%S')}'),\n"
            f.write(row)

# Esempio di utilizzo:
num_rows = 1000000  # Specifica il numero di righe
output_file = 'dataset.txt'  # Nome del file da generare
generate_file(num_rows, output_file)
