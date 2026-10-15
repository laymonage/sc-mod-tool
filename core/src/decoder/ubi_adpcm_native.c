#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "coding.h"

typedef int (*cancel_check_fn)(void* context);

typedef struct {
    STREAMFILE streamfile;
    const uint8_t* bytes;
    size_t size;
} memory_streamfile;

static size_t read_memory_streamfile(
    STREAMFILE* streamfile,
    uint8_t* destination,
    offv_t offset,
    size_t length
) {
    memory_streamfile* memory = (memory_streamfile*)streamfile;
    if (offset < 0 || (uint64_t)offset >= memory->size) {
        return 0;
    }

    size_t available = memory->size - (size_t)offset;
    if (length > available) {
        length = available;
    }
    memcpy(destination, memory->bytes + (size_t)offset, length);
    return length;
}

int decode_sc1_ubi_adpcm(
    const uint8_t* input,
    uint32_t input_size,
    int16_t* output,
    uint32_t output_samples,
    uint32_t* output_sample_rate,
    cancel_check_fn is_cancelled,
    void* cancel_context
) {
    if (!input || !output || !output_sample_rate || input_size < 0x30 || output_samples > INT32_MAX) {
        return -1;
    }

    if (
        get_u32le(input) != 0x08
        || get_u32le(input + 0x04) != output_samples
        || get_u32le(input + 0x10) == 0
        || (get_u32le(input + 0x24) != 4 && get_u32le(input + 0x24) != 6)
        || get_u32le(input + 0x2c) != 1
    ) {
        return -1;
    }

    memory_streamfile memory = {
        .streamfile = { .read = read_memory_streamfile },
        .bytes = input,
        .size = input_size
    };
    VGMSTREAMCHANNEL channel = { 0 };
    VGMSTREAM vgmstream = { 0 };
    channel.streamfile = &memory.streamfile;
    vgmstream.channels = 1;
    vgmstream.ch = &channel;
    vgmstream.codec_data = init_ubi_adpcm(&memory.streamfile, 0, input_size, 1);
    if (!vgmstream.codec_data) {
        return -1;
    }

    uint32_t samples_decoded = 0;
    while (samples_decoded < output_samples) {
        if (is_cancelled && is_cancelled(cancel_context)) {
            free_ubi_adpcm(vgmstream.codec_data);
            return -2;
        }
        uint32_t chunk = output_samples - samples_decoded;
        if (chunk > 2048) {
            chunk = 2048;
        }
        decode_ubi_adpcm(
            &vgmstream,
            output + samples_decoded,
            (int32_t)chunk
        );
        samples_decoded += chunk;
    }
    free_ubi_adpcm(vgmstream.codec_data);

    *output_sample_rate = get_u32le(input + 0x18);
    if (*output_sample_rate == 0) {
        *output_sample_rate = 36000;
    }
    return (int)output_samples;
}
