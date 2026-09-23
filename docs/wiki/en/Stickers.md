# Sticker panel

[简体中文](../zh-CN/Stickers.md) · [Guide](Home.md)

Press Ctrl-T in the composer to open the sticker panel. The panel lists your
Recent and Favorites stickers followed by every installed sticker set, and
sends the selected sticker as a native Telegram sticker — no re-upload, and
your draft text stays untouched.

| In the sticker panel | Action |
| --- | --- |
| hjkl or arrow keys | Move in the sticker grid |
| PageUp/PageDown, Home/End | Move through the stickers |
| Tab / Shift-Tab | Next / previous section |
| Mouse click | Select a section or sticker; click the selected sticker again to send |
| Enter or Space | Send the selected sticker |
| Ctrl-R | Refetch Recent, Favorites and the set list; retry a failed set |
| Esc or q | Close and return to the composer |

The left sidebar lists the sections: Recent, Favorites and one row per
installed set. Set contents load lazily when a section is first opened and stay
cached for the session; Recent, Favorites and the set list are refetched on
every panel open because Telegram's file references expire.

Grid cells render each sticker's static raster thumbnail with the same inline
image pipeline as message previews; cells whose thumbnail has not downloaded
yet show the sticker's emoji. Animated TGS/WebM stickers use their static
thumbnail, which is expected. While lists load, the panel shows a loading
notice; a failed fetch shows the error and Ctrl-R retries; empty sections say
so. Thumbnails download lazily, at most four transfers in flight, and are
reused from the media cache per document.

Sending honors the current reply: the sticker attaches the reply target and
clears the reply state, exactly like sending the draft, but the draft text is
preserved. An optimistic pending message appears immediately and is replaced
when Telegram confirms; a failed send marks only that message. Send
restrictions and slow mode are enforced like for text.

The entry action is `stickers` in the `compose` context (default Ctrl-T).
Panel keys use the `stickers` context with the actions `open`, `cancel`,
`refresh`, `up`, `down`, `left`, `right`, `page_up`, `page_down`, `home`,
`end`, `sticker_set_next` and `sticker_set_previous`.
