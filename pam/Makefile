CC ?= gcc
CFLAGS = -fPIC -Wall -Wextra -O2
TARGET = pam_immurok.so

all: $(TARGET)

$(TARGET): pam_immurok.c
	$(CC) $(CFLAGS) -shared -o $@ $< -lpam

clean:
	rm -f $(TARGET)
