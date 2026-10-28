---
title: Doing It Live
date: 2026-10-27
summary: What's all this then?
author: Steve
---

## Blogger

I haven't written a blog post in ages so bear with me! With the recent PHP expolit news I've definielty though back to my early days. One of my first sites was a hand rolled PHP blog. I know that some remnants from 2004/2005 made it into the Wayback Machine but all the backend code was lost to time. Oh and there was that _Tencho Muyo!_ fansite but I can't recall if that was PHP too, 1995/96 I think? Maybe, I guess it could have been. That one was even mentioned in the _Yahoo!_ magazine, yes they had a print magazine about websites. Anyway...

## What

So, what the heck is Threaded? Well, it's something that has been done before. Link sites exsist and they have audiences, there are even several dope ones on the Atmosphere already (see [this post](https://jola.dev/posts/building-atproto-link-aggregator)). We aren't really covering any "new" ground.

Threaded itself comes out of a thread on [Bluesky](https://bsky.app/profile/pfrazee.com/post/3mypohe7nbk2p) after serveral things happened across the interwebs. There was the rule about no AI translations and such in /opensource. And people disagreing about what should be tagged as what on lobste.rs.

And honestly do I want to get myself in the middle of that type of thing? Not really. But, I did want to see how smooth of a process it would be to use an LLM to design and build a site that could work as a link site but, leverage LLM moderation and of course pull from the Atmosphere and contribute back into it through the story submissions, comments, and votes (which are only public if you opt-in).

## Open

Yes, this is put together with LLM assistence and some heavy lifting. Claude specifically. My Rust is admittedly still week but I'm slowly getting the hang of it. Feel free to look at [the repo](https://github.com/shindakun/threaded.at) if you'd like. Or if that's not your thing, don't. That's cool. The plan right now is that if this project sees any use any moderation will be public. Within reason, I mean comments and stories may get hidden or removed but there will be an attempt to be a clear reason. Be civil, which may be asking a lot of the internet at large.

## Limiting

I've seen an agrument floated around how using LLMs can potantially limit a projects reach and adapton. And that's cool, see the thing is - a person doesn't have to use it if they don't want to or disapprove of how it's built. I still think there may be a difference between "vibecoded slop" and "using an llm". Jury is still out on where this project actually lands though.

## Next

Next steps are some cleaning up. Improving the reading experience. Then adding more Atmposphere type stuff. Also, I should move these posts into standard.site lexicons.
